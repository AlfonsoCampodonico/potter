use std::path::{Path, PathBuf};

use potter_config::{ConfigError, plan_repo};
use potter_plan::{Base, Origin, Step, Target};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name)
}

fn names(t: &Target) -> Vec<&str> {
    t.build.steps.iter().map(|s| s.name()).collect()
}

fn cmd(t: &Target, name: &str) -> String {
    match t.build.steps.iter().find(|s| s.name() == name).unwrap() {
        Step::Run { command, .. } => command.to_string(),
        other => panic!("{name} is {}", other.kind()),
    }
}

#[test]
fn overrides_apply_in_order() {
    let p = plan_repo(&fixture("overrides"), None).unwrap();
    let t = &p.plan.targets[0];
    assert_eq!(
        names(t),
        [
            "apt-build",
            "deps-inputs",
            "deps",
            "node-deps-inputs",
            "node-deps",
            "source",
            "autoload",
            "warm",
            "assets-inputs",
            "assets",
            "user-build-1"
        ]
    );
    assert_eq!(cmd(t, "assets"), "npm run build:prod");
    assert_eq!(cmd(t, "warm"), "php artisan icons:cache");
    assert_eq!(cmd(t, "user-build-1"), "php artisan route:list");
    match &t.build.steps[8] {
        Step::Inputs { include, .. } => assert_eq!(include, &["resources/**", "vite.config.js", "package.json"]),
        other => panic!("{other:?}"),
    }
    let Base::Catalog { layers } = &t.build.base else {
        panic!()
    };
    assert!(layers.iter().any(|l| l.to_string() == "php@8.3"));
    assert!(layers.iter().any(|l| l.to_string() == "php-ext-imagick@8.3"));
    assert_eq!(
        t.image.processes["web"].command.join(" "),
        "php artisan serve --host=0.0.0.0 --port=8080"
    );
    assert!(!t.image.services.contains_key("web"));
    assert_eq!(
        t.image.processes["deploy"].command,
        ["sh", "-c", "php artisan migrate --force && php artisan config:cache"]
    );
    assert!(
        matches!(&t.image.runtime_steps[0], Step::Apt { name, packages, snapshot } if name == "apt-runtime" && packages == &["libvips42"] && snapshot == "20261001T000000Z")
    );
    let ds = &p.decisions[0].1;
    assert!(ds.iter().any(|d| d.field == "steps.assets.command"
        && d.origin
            == Origin::Config {
                key: "target.app.steps.assets.run".into()
            }));
    assert!(ds.iter().any(|d| d.field == "steps.discover" && d.value == "skipped"));
}

#[test]
fn explicit_plan() {
    let p = plan_repo(&fixture("explicit-go"), None).unwrap();
    let t = &p.plan.targets[0];
    assert_eq!(t.name, "api");
    assert_eq!(t.provider, "none");
    assert_eq!(names(t), ["source", "build"]);
    let Base::Catalog { layers } = &t.image.base else {
        panic!()
    };
    assert_eq!(layers.iter().map(|l| l.to_string()).collect::<Vec<_>>(), ["os-min"]);
    assert_eq!(t.image.export[0].path, "/out/api");
    assert_eq!(t.image.config.port, Some(8080));
    match &t.build.steps[1] {
        Step::Run { cache_mounts, .. } => {
            assert_eq!(
                cache_mounts.iter().map(|m| m.target.as_str()).collect::<Vec<_>>(),
                ["/cache/gomod", "/cache/gobuild"]
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ambiguous_root_names_the_target() {
    let msg = plan_repo(&fixture("ambiguous"), None).unwrap_err().to_string();
    assert!(
        msg.starts_with("target `app`: laravel, node all claim the build root"),
        "{msg}"
    );
}

fn summary(p: &potter_config::PlannedRepo) -> Vec<(String, String, String)> {
    p.plan
        .targets
        .iter()
        .map(|t| (t.name.clone(), t.root.clone(), t.provider.clone()))
        .collect()
}

fn owned(v: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    v.iter()
        .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
        .collect()
}

#[test]
fn monorepo_root_discovers_targets() {
    let p = plan_repo(&fixture("monorepo-root"), None).unwrap();
    assert_eq!(
        summary(&p),
        owned(&[("backend", "backend", "go"), ("frontend", "frontend", "node")])
    );
    let backend = &p.decisions[0].1;
    assert!(
        backend
            .iter()
            .any(|d| d.field == "target" && d.origin.to_string().contains("backend/ has a manifest"))
    );
    assert_eq!(
        plan_repo(&fixture("monorepo-root"), Some("frontend"))
            .unwrap()
            .plan
            .targets
            .len(),
        1
    );
}

#[test]
fn explicit_monorepo_targets() {
    let p = plan_repo(&fixture("monorepo-targets"), None).unwrap();
    assert_eq!(
        summary(&p),
        owned(&[("frontend", "frontend", "node"), ("backend", "backend", "go")])
    );
}

#[test]
fn apps_and_services_are_discovered() {
    let d = temp(&[
        ("apps/Web App/package.json", r#"{"scripts":{"start":"node server.js"}}"#),
        ("apps/Web App/package-lock.json", "{}"),
        ("services/api/go.mod", "module api\n\ngo 1.26\n"),
        ("services/api/main.go", "package main\n"),
        ("README.md", "monorepo"),
    ]);
    let p = plan_repo(d.path(), None).unwrap();
    assert_eq!(
        summary(&p),
        owned(&[
            ("apps-web-app", "apps/Web App", "node"),
            ("services-api", "services/api", "go")
        ])
    );
}

#[test]
fn a_root_provider_disables_discovery() {
    let d = temp(&[
        ("composer.json", r#"{"require":{}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
        ("tools/package.json", "{}"),
    ]);
    let p = plan_repo(d.path(), None).unwrap();
    assert_eq!(summary(&p), owned(&[("app", ".", "php")]));
}

#[test]
fn discovered_target_failure_names_the_target() {
    let d = temp(&[
        ("web/package.json", r#"{"scripts":{"start":"x"}}"#),
        ("api/go.mod", "module a\n\ngo 1.26\n"),
        ("api/main.go", "package main\n"),
    ]);
    let msg = plan_repo(d.path(), None).unwrap_err().to_string();
    assert!(
        msg.starts_with("target `web` (discovered automatically from web/):") && msg.contains("lockfile"),
        "{msg}"
    );
}

#[test]
fn target_filter() {
    assert!(matches!(
        plan_repo(&fixture("explicit-go"), Some("nope")),
        Err(ConfigError::UnknownTarget { .. })
    ));
    assert_eq!(
        plan_repo(&fixture("explicit-go"), Some("api"))
            .unwrap()
            .plan
            .targets
            .len(),
        1
    );
}

fn temp(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (p, c) in files {
        let path = d.path().join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, c).unwrap();
    }
    d
}

#[test]
fn override_errors() {
    let php = [
        ("composer.json", r#"{"require":{}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
    ];
    let cases = [
        ("[target.app.steps.nope]\nrun = \"x\"\n", "target.app.steps.nope"),
        (
            "[target.app.steps.new]\nafter = \"missing\"\nrun = \"x\"\n",
            "target.app.steps.new.after",
        ),
        ("[target.app.steps.new]\nafter = \"deps\"\n", "target.app.steps.new"),
        (
            "[target.app.steps.deps]\nafter = \"source\"\n",
            "target.app.steps.deps.after",
        ),
        ("[target.app.process.web]\ncmd = []\n", "target.app.process.web.cmd"),
        (
            "[target.app.steps.deps]\ncache = [\"Bad\"]\n",
            "target.app.steps.deps.cache",
        ),
    ];
    for (toml, key) in cases {
        let d = temp(&[php[0], php[1], ("potter.toml", toml)]);
        match plan_repo(d.path(), None) {
            Err(ConfigError::InTarget { source, .. }) => match *source {
                ConfigError::Invalid { key: k, .. } => assert_eq!(k, key, "{toml}"),
                other => panic!("{toml}: {other}"),
            },
            other => panic!("{toml}: {other:?}"),
        }
    }
}

#[test]
fn invalid_plans_from_config_are_caught() {
    let d = temp(&[
        ("composer.json", r#"{"require":{}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
        ("potter.toml", "[apt]\nbuild = [\"Not A Package\"]\n"),
    ]);
    assert!(matches!(plan_repo(d.path(), None), Err(ConfigError::Plan(_))));
}

#[test]
fn explicit_plan_needs_a_base_and_a_process() {
    let d = temp(&[(
        "potter.toml",
        "[target.x]\nprovider = \"none\"\n[target.x.process.web]\ncmd = [\"/bin/x\"]\n",
    )]);
    assert!(
        plan_repo(d.path(), None)
            .unwrap_err()
            .to_string()
            .contains("target.x.base")
    );
    let d = temp(&[(
        "potter.toml",
        "[target.x]\nprovider = \"none\"\n[target.x.base]\noci = \"debian:13\"\n",
    )]);
    assert!(
        plan_repo(d.path(), None)
            .unwrap_err()
            .to_string()
            .contains("target.x.process")
    );
}

#[cfg(unix)]
#[test]
fn target_root_through_a_symlink_is_refused() {
    let outside = temp(&[
        ("composer.json", r#"{"require":{}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
        ("sub/composer.json", r#"{"require":{}}"#),
        ("sub/composer.lock", r#"{"packages":[]}"#),
    ]);
    for (toml, link) in [
        ("[target.api]\nroot = \"api\"\n", "api"),
        ("[target.api]\nroot = \"link/sub\"\n", "link"),
    ] {
        let d = temp(&[("potter.toml", toml)]);
        std::os::unix::fs::symlink(outside.path(), d.path().join(link)).unwrap();
        match plan_repo(d.path(), None) {
            Err(ConfigError::InTarget { source, .. }) => match *source {
                ConfigError::Invalid { key, reason } => {
                    assert_eq!(key, "target.api.root", "{toml}");
                    assert!(reason.contains("symlink"), "{reason}");
                }
                other => panic!("{toml}: {other}"),
            },
            other => panic!("{toml}: expected a refusal, got {other:?}"),
        }
    }
}

#[test]
fn missing_target_root_is_a_config_error() {
    let d = temp(&[("potter.toml", "[target.api]\nroot = \"nope\"\n")]);
    match plan_repo(d.path(), None) {
        Err(ConfigError::InTarget { source, .. }) => assert!(matches!(*source, ConfigError::Invalid { .. })),
        other => panic!("{other:?}"),
    }
}

#[test]
fn new_steps_with_the_same_anchor_keep_file_order() {
    let d = temp(&[
        ("composer.json", r#"{"require":{}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
        (
            "potter.toml",
            "[target.app.steps.first]\nafter = \"deps\"\nrun = \"a\"\n[target.app.steps.second]\nafter = \"deps\"\nrun = \"b\"\n",
        ),
    ]);
    let p = plan_repo(d.path(), None).unwrap();
    assert_eq!(
        names(&p.plan.targets[0]),
        ["deps-inputs", "deps", "first", "second", "source", "autoload"]
    );
}

#[test]
fn export_extra_paths_for_detected_targets() {
    let d = temp(&[
        ("go.mod", "module x\n\ngo 1.26\n"),
        ("main.go", "package main\n"),
        ("data/seed.json", "{}"),
        ("potter.toml", "[target.app]\nexport = [\"data\", \"/etc/app.conf\"]\n"),
    ]);
    let p = plan_repo(d.path(), None).unwrap();
    let exports: Vec<(&str, &str)> = p.plan.targets[0]
        .image
        .export
        .iter()
        .map(|e| (e.path.as_str(), e.dest.as_str()))
        .collect();
    assert_eq!(
        exports,
        [
            ("/out", "/out"),
            ("/app/data", "/out/data"),
            ("/etc/app.conf", "/etc/app.conf")
        ]
    );
    assert!(
        p.decisions[0]
            .1
            .iter()
            .any(|d| d.field == "export" && d.value == "data")
    );
}

#[test]
fn export_rejects_escapes() {
    let d = temp(&[
        ("go.mod", "module x\n\ngo 1.26\n"),
        ("main.go", "package main\n"),
        ("potter.toml", "[target.app]\nexport = [\"../x\"]\n"),
    ]);
    match plan_repo(d.path(), None) {
        Err(ConfigError::InTarget { source, .. }) => {
            assert!(
                matches!(*source, ConfigError::Invalid { ref key, .. } if key == "target.app.export"),
                "{source}"
            )
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn web_cmd_resolves_ambiguous_go_mains() {
    let d = temp(&[
        ("go.mod", "module x\n\ngo 1.26\n"),
        ("cmd/alpha/main.go", "package main\n"),
        ("cmd/beta/main.go", "package main\n"),
        (
            "potter.toml",
            "[target.app.process.web]\ncmd = [\"/out/alpha\", \"serve\"]\n",
        ),
    ]);
    let t = &plan_repo(d.path(), None).unwrap().plan.targets[0];
    assert_eq!(t.image.processes["web"].command, ["/out/alpha", "serve"]);
    assert_eq!(t.image.processes["alpha"].command, ["/out/alpha"]);
    assert_eq!(t.image.processes["beta"].command, ["/out/beta"]);
}

#[test]
fn web_cmd_resolves_missing_framework_and_start_script() {
    let d = temp(&[
        ("requirements.txt", "requests\n"),
        (
            "potter.toml",
            "[target.app.process.web]\ncmd = [\"/app/.venv/bin/python\", \"worker.py\"]\n",
        ),
    ]);
    let t = &plan_repo(d.path(), None).unwrap().plan.targets[0];
    assert_eq!(t.image.processes["web"].command, ["/app/.venv/bin/python", "worker.py"]);
    let d = temp(&[
        ("package.json", r#"{"dependencies":{"express":"^5"}}"#),
        ("package-lock.json", "{}"),
        (
            "potter.toml",
            "[target.app.process.web]\ncmd = [\"node\", \"server.js\"]\n",
        ),
    ]);
    let t = &plan_repo(d.path(), None).unwrap().plan.targets[0];
    assert_eq!(t.image.processes["web"].command, ["node", "server.js"]);
}

#[test]
fn auxiliary_directories_are_not_discovered() {
    let d = temp(&[
        ("backend/go.mod", "module b\n\ngo 1.26\n"),
        ("backend/main.go", "package main\n"),
        ("docs/requirements.txt", "mkdocs\n"),
        ("tools/package.json", "{}"),
        (".github/package.json", "{}"),
    ]);
    let p = plan_repo(d.path(), None).unwrap();
    assert_eq!(summary(&p), owned(&[("backend", "backend", "go")]));
}

#[test]
fn discovered_failures_say_they_were_discovered() {
    let d = temp(&[
        ("web/package.json", r#"{"scripts":{"start":"x"}}"#),
        ("api/go.mod", "module a\n\ngo 1.26\n"),
        ("api/main.go", "package main\n"),
    ]);
    let msg = plan_repo(d.path(), None).unwrap_err().to_string();
    assert!(
        msg.contains("discovered automatically") && msg.contains("discover = false"),
        "{msg}"
    );
}

#[test]
fn declared_targets_merge_with_discovered_ones() {
    let d = temp(&[
        ("frontend/package.json", r#"{"scripts":{"start":"next start"}}"#),
        ("frontend/package-lock.json", "{}"),
        ("backend/go.mod", "module b\n\ngo 1.26\n"),
        ("backend/main.go", "package main\n"),
        ("potter.toml", "[target.backend]\nport = 9000\n"),
    ]);
    let p = plan_repo(d.path(), None).unwrap();
    assert_eq!(
        summary(&p),
        owned(&[("backend", "backend", "go"), ("frontend", "frontend", "node")])
    );
    assert_eq!(p.plan.targets[0].image.config.port, Some(9000));
    let d = temp(&[
        ("frontend/package.json", r#"{"scripts":{"start":"next start"}}"#),
        ("frontend/package-lock.json", "{}"),
        ("backend/go.mod", "module b\n\ngo 1.26\n"),
        ("backend/main.go", "package main\n"),
        (
            "potter.toml",
            "discover = false\n[target.backend]\nroot = \"backend\"\n",
        ),
    ]);
    assert_eq!(
        summary(&plan_repo(d.path(), None).unwrap()),
        owned(&[("backend", "backend", "go")])
    );
}
