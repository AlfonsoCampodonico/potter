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
            "assets-inputs",
            "assets",
            "autoload",
            "warm",
            "user-build-1"
        ]
    );
    assert_eq!(cmd(t, "assets"), "npm run build:prod");
    assert_eq!(cmd(t, "warm"), "php artisan icons:cache");
    assert_eq!(cmd(t, "user-build-1"), "php artisan route:list");
    match &t.build.steps[6] {
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

#[test]
fn monorepo_root_hint_and_targets() {
    let msg = plan_repo(&fixture("monorepo-root"), None).unwrap_err().to_string();
    assert!(msg.contains("frontend/") && msg.contains("backend/"), "{msg}");
    let p = plan_repo(&fixture("monorepo-targets"), None).unwrap();
    let t = &p.plan.targets[0];
    assert_eq!(
        (t.name.as_str(), t.root.as_str(), t.provider.as_str()),
        ("frontend", "frontend", "node")
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
