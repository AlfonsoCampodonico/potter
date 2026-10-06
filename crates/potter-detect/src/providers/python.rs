//! Python applications (spec §6.1 "Python"). Dependencies go into a virtualenv at /app/.venv,
//! which ships in the image; processes call its binaries by absolute path.

use std::collections::BTreeSet;

use potter_plan::{Command, DEFAULT_APT_SNAPSHOT, Decision, ImageConfig, Origin, Process, Step};

use crate::catalog::{self, Line, PYTHON, VersionSource};
use crate::options::DetectOptions;
use crate::providers::common::*;
use crate::providers::{Detection, Provider};
use crate::repo::{LOCKFILE_CAP, READ_CAP, RepoView};
use crate::scan::quoted_value_after;
use crate::select::{Claim, ClaimKind};
use crate::version::{VersionSet, parse_pep440, parse_poetry_constraint, parse_version_file};
use crate::{DetectError, ProviderId};

const VENV: &str = "/app/.venv";
const MARKERS: &[&str] = &[
    "pyproject.toml",
    "requirements.txt",
    "Pipfile",
    "uv.lock",
    "poetry.lock",
];
const APP_FILES: &[&str] = &[
    "main.py",
    "app.py",
    "app/main.py",
    "src/main.py",
    "application.py",
    "server.py",
    "wsgi.py",
    "asgi.py",
];
/// Python packages that need system libraries: (package, build packages, runtime packages).
const APT_TABLE: &[(&str, &[&str], &[&str])] = &[
    ("psycopg2", &["libpq-dev"], &["libpq5"]),
    (
        "mysqlclient",
        &["default-libmysqlclient-dev", "pkg-config"],
        &["libmariadb3"],
    ),
];
const MAX_INCLUDE_DEPTH: usize = 5;

pub(crate) struct PythonProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pm {
    Uv,
    Poetry,
    Pipenv,
    Pip,
}

impl Pm {
    fn name(self) -> &'static str {
        match self {
            Pm::Uv => "uv",
            Pm::Poetry => "poetry",
            Pm::Pipenv => "pipenv",
            Pm::Pip => "pip",
        }
    }

    fn lockfile(self) -> &'static str {
        match self {
            Pm::Uv => "uv.lock",
            Pm::Poetry => "poetry.lock",
            Pm::Pipenv => "Pipfile.lock",
            Pm::Pip => "requirements.txt",
        }
    }

    fn parse(s: &str) -> Option<Pm> {
        [Pm::Uv, Pm::Poetry, Pm::Pipenv, Pm::Pip]
            .into_iter()
            .find(|p| p.name() == s)
    }
}

impl Provider for PythonProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Python
    }

    fn claim(&self, repo: &RepoView, _opts: &DetectOptions) -> Result<Option<Claim>, DetectError> {
        Ok(MARKERS.iter().find(|m| repo.is_file(m)).map(|m| Claim {
            provider: ProviderId::Python,
            refines: None,
            kind: ClaimKind::Backend,
            evidence: vec![m.to_string()],
        }))
    }

    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
        let mut d = Vec::new();
        let pm = package_manager(repo, opts, &mut d)?;
        let pyproject = repo.read_toml("pyproject.toml")?;
        let python = python_version(repo, opts, pyproject.as_ref(), &mut d)?;
        let deps = dependencies(repo, pm, pyproject.as_ref())?;

        let mut build_layers = vec![cref("os-min", None), cref("python", Some(python.version))];
        if pm != Pm::Pip {
            build_layers.push(cref(pm.name(), None));
        }
        let mut steps = Vec::new();
        let (apt_build, apt_runtime) = apt_packages(&deps.names);
        if !apt_build.is_empty() {
            d.push(Decision::detected(
                "apt.build",
                apt_build.join(" "),
                ["Python packages that need system libraries"],
            ));
            steps.push(Step::Apt {
                name: "apt-detected".into(),
                packages: apt_build,
                snapshot: DEFAULT_APT_SNAPSHOT.into(),
            });
        }
        let (env, cache) = pm_env(pm);
        steps.push(inputs("deps-inputs", deps.inputs.clone()));
        steps.push(run("deps", install_command(pm), &env, &cache));
        steps.push(source());
        match pm {
            Pm::Uv => steps.push(run(
                "project",
                Command::argv(&["uv", "sync", "--frozen", "--no-dev"]),
                &env,
                &cache,
            )),
            Pm::Poetry => steps.push(run(
                "project",
                Command::shell("poetry install --only main --no-interaction"),
                &env,
                &cache,
            )),
            Pm::Pipenv | Pm::Pip => {}
        }

        let port = port(opts, 8000, &mut d);
        let web = web_process(repo, &deps.names, port, opts, &mut d)?;
        if let Some(project) = &web.django_project {
            let enabled = opts.python.collectstatic.unwrap_or(true);
            if enabled {
                steps.push(run(
                    "collectstatic",
                    Command::argv(&[&format!("{VENV}/bin/python"), "manage.py", "collectstatic", "--noinput"]),
                    &[],
                    &[],
                ));
                d.push(Decision::detected(
                    "steps.collectstatic",
                    "enabled",
                    [format!("Django project {project}")],
                ));
            } else {
                d.push(Decision::config(
                    "steps.collectstatic",
                    "disabled",
                    opts.key("python.django.collectstatic"),
                ));
            }
        }

        let mut config = ImageConfig::new("/app", "root");
        config.port = Some(port);
        config.env.insert("PORT".into(), port.to_string());
        config.env.insert("PYTHONUNBUFFERED".into(), "1".into());
        config.env.insert("VIRTUAL_ENV".into(), VENV.into());
        config
            .env
            .insert("PATH".into(), format!("{VENV}/bin:/usr/local/bin:/usr/bin:/bin"));
        let run_layers = vec![cref("os-min", None), cref("python", Some(python.version))];
        let mut image = empty_image(run_layers, config);
        if !apt_runtime.is_empty() {
            image.runtime_steps.push(Step::Apt {
                name: "apt-detected-runtime".into(),
                packages: apt_runtime,
                snapshot: DEFAULT_APT_SNAPSHOT.into(),
            });
        }
        image.export = vec![export("/app", "/app")];
        image.processes.insert(
            "web".into(),
            Process {
                command: web.command,
                workdir: None,
                env: Default::default(),
            },
        );
        Ok(Detection {
            target: new_target(target, ProviderId::Python, build_layers, steps, image),
            decisions: d,
        })
    }
}

fn package_manager(repo: &RepoView, opts: &DetectOptions, d: &mut Vec<Decision>) -> Result<Pm, DetectError> {
    let all = [Pm::Uv, Pm::Poetry, Pm::Pipenv, Pm::Pip];
    if let Some(name) = &opts.python.package_manager {
        let key = opts.key("python.package_manager");
        let pm = Pm::parse(name).ok_or_else(|| DetectError::NeedsConfig {
            provider: ProviderId::Python,
            message: format!("unknown package manager `{name}` (uv, poetry, pipenv or pip)"),
            key: key.clone(),
        })?;
        if !repo.is_file(pm.lockfile()) {
            return Err(DetectError::MissingFile {
                provider: ProviderId::Python,
                file: pm.lockfile().into(),
                hint: format!("{name} is the package manager but {} is missing", pm.lockfile()),
            });
        }
        d.push(Decision::config("python.package_manager", pm.name(), key));
        return Ok(pm);
    }
    let present: Vec<Pm> = all.into_iter().filter(|p| repo.is_file(p.lockfile())).collect();
    let Some(&chosen) = present.first() else {
        return Err(DetectError::MissingFile {
            provider: ProviderId::Python,
            file: "a lockfile".into(),
            hint: "commit uv.lock, poetry.lock, Pipfile.lock or requirements.txt so builds are reproducible".into(),
        });
    };
    let evidence = if present.len() > 1 {
        let files: Vec<&str> = present.iter().map(|p| p.lockfile()).collect();
        format!(
            "several lockfiles ({}); chose {}; set {} to change",
            files.join(", "),
            chosen.name(),
            opts.key("python.package_manager")
        )
    } else {
        format!("{} is present", chosen.lockfile())
    };
    d.push(Decision::detected("python.package_manager", chosen.name(), [evidence]));
    Ok(chosen)
}

fn detected(desc: String, set: VersionSet) -> VersionSource {
    VersionSource {
        desc: desc.clone(),
        origin: Origin::Detected { evidence: vec![desc] },
        set,
        lts_only: false,
    }
}

fn python_version(
    repo: &RepoView,
    opts: &DetectOptions,
    pyproject: Option<&toml::Value>,
    d: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let mut src = version_from_config(opts.runtime.python.as_deref(), &opts.key("runtime.python"), "python")?;
    if src.is_none() {
        src = version_from_file(repo, ".python-version", "python")?;
    }
    if src.is_none()
        && let Some(spec) = pyproject.and_then(|p| p.get("project")?.get("requires-python")?.as_str())
    {
        let desc = format!("pyproject.toml project.requires-python = {spec}");
        let set = parse_pep440(spec).map_err(|e| bad_constraint("python", spec, &desc, e))?;
        src = Some(detected(desc, set));
    }
    if src.is_none()
        && let Some(spec) = pyproject.and_then(|p| {
            p.get("tool")?
                .get("poetry")?
                .get("dependencies")?
                .get("python")?
                .as_str()
        })
    {
        let desc = format!("pyproject.toml tool.poetry.dependencies.python = {spec}");
        let set = parse_poetry_constraint(spec).map_err(|e| bad_constraint("python", spec, &desc, e))?;
        src = Some(detected(desc, set));
    }
    if src.is_none()
        && let Some(text) = repo.read_string("runtime.txt", READ_CAP)?
    {
        let raw = text.trim();
        let version = raw.strip_prefix("python-").unwrap_or(raw);
        let desc = format!("runtime.txt = {raw}");
        let (set, _) = parse_version_file(version).map_err(|e| bad_constraint("python", raw, "runtime.txt", e))?;
        src = Some(detected(desc, set));
    }
    catalog::pick(&PYTHON, "runtime.python", src, d)
}

struct Deps {
    /// Normalised distribution names (lower case, `-` separators).
    names: BTreeSet<String>,
    /// Files the dependency install reads.
    inputs: Vec<String>,
}

fn normalize(name: &str) -> String {
    name.to_ascii_lowercase().replace(['_', '.'], "-")
}

/// The distribution name at the start of a PEP 508 requirement, if it is one.
fn requirement_name(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(['#', '-', '.', '/']) || line.contains("://") {
        return None;
    }
    let end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || "._-".contains(c)))
        .unwrap_or(line.len());
    (end > 0).then(|| normalize(&line[..end]))
}

fn add_requirement(line: &str, names: &mut BTreeSet<String>) {
    if let Some(name) = requirement_name(line) {
        // `fastapi[standard]` brings uvicorn with it.
        if name == "fastapi" && line.contains("standard") {
            names.insert("uvicorn".into());
        }
        names.insert(name);
    }
}

fn dependencies(repo: &RepoView, pm: Pm, pyproject: Option<&toml::Value>) -> Result<Deps, DetectError> {
    let mut deps = Deps {
        names: BTreeSet::new(),
        inputs: Vec::new(),
    };
    if let Some(p) = pyproject {
        if let Some(list) = p
            .get("project")
            .and_then(|x| x.get("dependencies"))
            .and_then(|x| x.as_array())
        {
            for dep in list.iter().filter_map(|v| v.as_str()) {
                add_requirement(dep, &mut deps.names);
            }
        }
        if let Some(table) = p
            .get("tool")
            .and_then(|x| x.get("poetry"))
            .and_then(|x| x.get("dependencies"))
            .and_then(|x| x.as_table())
        {
            deps.names
                .extend(table.keys().filter(|k| *k != "python").map(|k| normalize(k)));
        }
    }
    match pm {
        Pm::Uv => deps.inputs = vec!["pyproject.toml".into(), "uv.lock".into()],
        Pm::Poetry => {
            deps.inputs = vec!["pyproject.toml".into(), "poetry.lock".into()];
            if repo.is_file("poetry.toml") {
                deps.inputs.push("poetry.toml".into());
            }
        }
        Pm::Pipenv => {
            deps.inputs = vec!["Pipfile".into(), "Pipfile.lock".into()];
            if let Some(lock) = repo.read_json_capped("Pipfile.lock", LOCKFILE_CAP)?
                && let Some(default) = lock.get("default").and_then(|v| v.as_object())
            {
                deps.names.extend(default.keys().map(|k| normalize(k)));
            }
        }
        Pm::Pip => read_requirements(repo, "requirements.txt", 0, &mut deps)?,
    }
    Ok(deps)
}

/// Reads a requirements file and the files it includes with `-r`/`-c`, which the install also needs.
fn read_requirements(repo: &RepoView, rel: &str, depth: usize, deps: &mut Deps) -> Result<(), DetectError> {
    if depth > MAX_INCLUDE_DEPTH || deps.inputs.iter().any(|i| i == rel) {
        return Ok(());
    }
    let Some(text) = repo.read_string(rel, READ_CAP)? else {
        return Err(DetectError::MissingFile {
            provider: ProviderId::Python,
            file: rel.into(),
            hint: "it is referenced by another requirements file".into(),
        });
    };
    deps.inputs.push(rel.to_string());
    for raw in text.lines() {
        let line = raw.split(" #").next().unwrap_or("").trim();
        let include = ["-r", "--requirement", "-c", "--constraint"].iter().find_map(|flag| {
            line.strip_prefix(flag)
                .filter(|r| r.starts_with([' ', '=']))
                .map(|r| r[1..].trim())
        });
        match include {
            Some(path) => {
                let dir = rel.rsplit_once('/').map(|(d, _)| d);
                let joined = match dir {
                    Some(d) => format!("{d}/{path}"),
                    None => path.to_string(),
                };
                if !potter_plan::is_relative_path(&joined) || joined.split('/').any(|c| c == "." || c.is_empty()) {
                    return Err(DetectError::Parse {
                        file: rel.into(),
                        reason: format!("includes `{path}`, which is not a plain path inside the build root"),
                    });
                }
                read_requirements(repo, &joined, depth + 1, deps)?;
            }
            None => add_requirement(line, &mut deps.names),
        }
    }
    Ok(())
}

fn apt_packages(names: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
    let mut build = Vec::new();
    let mut runtime = Vec::new();
    for (package, b, r) in APT_TABLE {
        if names.contains(*package) {
            build.extend(b.iter().map(|s| s.to_string()));
            runtime.extend(r.iter().map(|s| s.to_string()));
        }
    }
    (build, runtime)
}

/// (env, cache mounts) as (name, value) pairs.
type Pairs = Vec<(&'static str, &'static str)>;

fn pm_env(pm: Pm) -> (Pairs, Pairs) {
    match pm {
        Pm::Uv => (
            vec![
                ("UV_PROJECT_ENVIRONMENT", VENV),
                ("UV_CACHE_DIR", "/cache/uv"),
                ("UV_PYTHON_DOWNLOADS", "never"),
                ("UV_LINK_MODE", "copy"),
            ],
            vec![("uv", "/cache/uv")],
        ),
        Pm::Poetry => (
            vec![
                ("POETRY_VIRTUALENVS_IN_PROJECT", "true"),
                ("POETRY_CACHE_DIR", "/cache/poetry"),
            ],
            vec![("poetry", "/cache/poetry")],
        ),
        Pm::Pipenv => (
            vec![("PIPENV_VENV_IN_PROJECT", "1"), ("PIP_CACHE_DIR", "/cache/pip")],
            vec![("pip", "/cache/pip")],
        ),
        Pm::Pip => (vec![("PIP_CACHE_DIR", "/cache/pip")], vec![("pip", "/cache/pip")]),
    }
}

fn install_command(pm: Pm) -> Command {
    match pm {
        Pm::Uv => Command::argv(&["uv", "sync", "--frozen", "--no-dev", "--no-install-project"]),
        Pm::Poetry => Command::shell("poetry install --only main --no-root --no-interaction"),
        Pm::Pipenv => Command::shell("pipenv sync"),
        Pm::Pip => {
            Command::shell("python -m venv /app/.venv && /app/.venv/bin/pip install --no-input -r requirements.txt")
        }
    }
}

struct Web {
    command: Vec<String>,
    django_project: Option<String>,
}

/// A file defines or re-exports a module-level `app`.
fn binds_app(text: &str) -> bool {
    text.lines().map(str::trim_start).any(|l| {
        let assigns = l
            .strip_prefix("app")
            .is_some_and(|r| r.trim_start().starts_with(['=', ':']));
        let imports = l.starts_with("from ")
            && l.split_once(" import ")
                .is_some_and(|(_, names)| names.split(',').any(|n| n.split_whitespace().last() == Some("app")));
        assigns || imports
    })
}

fn app_module(repo: &RepoView) -> Result<Option<(String, &'static str)>, DetectError> {
    for f in APP_FILES {
        if let Some(text) = repo.read_string(f, READ_CAP)?
            && binds_app(&text)
        {
            return Ok(Some((f.trim_end_matches(".py").replace('/', "."), f)));
        }
    }
    Ok(None)
}

fn django_project(repo: &RepoView) -> Result<Option<String>, DetectError> {
    let Some(text) = repo.read_string("manage.py", READ_CAP)? else {
        return Ok(None);
    };
    let module = quoted_value_after(&text, "DJANGO_SETTINGS_MODULE", ',');
    let project = module
        .as_deref()
        .and_then(|m| m.split('.').next())
        .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
    Ok(project.map(str::to_string))
}

fn bin(name: &str) -> String {
    format!("{VENV}/bin/{name}")
}

fn web_process(
    repo: &RepoView,
    names: &BTreeSet<String>,
    port: u16,
    opts: &DetectOptions,
    d: &mut Vec<Decision>,
) -> Result<Web, DetectError> {
    let has = |n: &str| names.contains(n);
    let p = port.to_string();
    let no_process = |why: &str| DetectError::NoProcess {
        provider: ProviderId::Python,
        hint: format!("{why}, or set `{}` in potter.toml", opts.key("process.web.cmd")),
    };
    if has("django") && repo.is_file("manage.py") {
        let project =
            django_project(repo)?.ok_or_else(|| no_process("manage.py doesn't name DJANGO_SETTINGS_MODULE"))?;
        let command = if has("gunicorn") {
            vec![
                bin("gunicorn"),
                format!("{project}.wsgi"),
                "--bind".into(),
                format!("0.0.0.0:{p}"),
            ]
        } else if has("uvicorn") {
            vec![
                bin("uvicorn"),
                format!("{project}.asgi:application"),
                "--host".into(),
                "0.0.0.0".into(),
                "--port".into(),
                p,
            ]
        } else {
            return Err(no_process("add gunicorn or uvicorn to the dependencies"));
        };
        d.push(Decision::detected(
            "process.web",
            command.join(" "),
            [format!("Django project {project}")],
        ));
        return Ok(Web {
            command,
            django_project: Some(project),
        });
    }
    let asgi = ["fastapi", "starlette", "litestar", "quart"].iter().any(|n| has(n));
    let wsgi = ["flask", "falcon", "bottle"].iter().any(|n| has(n));
    if !(asgi || wsgi) {
        return Err(no_process(
            "no Django, FastAPI, Starlette or Flask dependency was found",
        ));
    }
    let (module, file) = app_module(repo)?.ok_or_else(|| no_process("no module defining `app` was found"))?;
    let target = format!("{module}:app");
    let command = match (asgi, has("uvicorn"), has("granian"), has("gunicorn")) {
        (true, true, _, _) => {
            vec![
                bin("uvicorn"),
                target,
                "--host".into(),
                "0.0.0.0".into(),
                "--port".into(),
                p,
            ]
        }
        (_, _, true, _) => vec![
            bin("granian"),
            "--interface".into(),
            if asgi { "asgi" } else { "wsgi" }.into(),
            "--host".into(),
            "0.0.0.0".into(),
            "--port".into(),
            p,
            target,
        ],
        (false, _, _, true) => vec![bin("gunicorn"), target, "--bind".into(), format!("0.0.0.0:{p}")],
        _ => {
            let servers = if asgi {
                "uvicorn or granian"
            } else {
                "gunicorn or granian"
            };
            return Err(no_process(&format!("add {servers} to the dependencies")));
        }
    };
    d.push(Decision::detected(
        "process.web",
        command.join(" "),
        [format!("{file} defines `app`")],
    ));
    Ok(Web {
        command,
        django_project: None,
    })
}
