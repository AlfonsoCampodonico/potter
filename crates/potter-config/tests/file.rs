use potter_config::ConfigError;
use potter_config::file::{load, parse, targets};
use potter_config::options::{Mode, options_for};
use potter_detect::ProviderId;

const SPEC_EXAMPLE: &str = r#"
[target.app]
root = "."
provider = "laravel"

[target.app.runtime]
php = "8.4"
extensions = ["imagick"]
node = "22"

[target.app.steps.assets]
run = "npm run build:prod"
inputs = ["resources/**", "vite.config.ts", "package.json"]

[target.app.process.web]
cmd = ["php", "artisan", "octane:start", "--server=frankenphp", "--port=8080"]

[target.api]
root = "services/api"
provider = "node"

[apt]
build = ["libvips-dev"]
runtime = ["libvips42"]
"#;

#[test]
fn parses_the_spec_example() {
    let cfg = parse(SPEC_EXAMPLE).unwrap();
    let t = targets(&cfg);
    assert_eq!(t.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["app", "api"]);
    let app = &cfg.target["app"];
    assert_eq!(app.runtime.extensions, ["imagick"]);
    assert_eq!(app.steps["assets"].run.as_deref(), Some("npm run build:prod"));
    assert_eq!(cfg.apt.runtime, ["libvips42"]);
}

#[test]
fn step_order_is_preserved() {
    let cfg = parse("[target.x.steps.zeta]\nrun = \"a\"\n[target.x.steps.alpha]\nrun = \"b\"\n").unwrap();
    assert_eq!(cfg.target["x"].steps.keys().collect::<Vec<_>>(), ["zeta", "alpha"]);
}

#[test]
fn unknown_keys_are_errors() {
    let e = parse("[target.app]\nprovidr = \"php\"\n").unwrap_err();
    assert!(matches!(&e, ConfigError::Parse(m) if m.contains("providr")), "{e}");
}

#[test]
fn implicit_single_target() {
    let t = targets(&parse("").unwrap());
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].0, "app");
}

#[test]
fn load_missing_and_oversized() {
    let d = tempfile::tempdir().unwrap();
    assert!(load(d.path()).unwrap().target.is_empty());
    std::fs::write(d.path().join("potter.toml"), vec![b'#'; 2 * 1024 * 1024]).unwrap();
    assert!(matches!(load(d.path()), Err(ConfigError::TooLarge(_))));
}

#[test]
fn options_mapping_and_validation() {
    let cfg = parse(SPEC_EXAMPLE).unwrap();
    let Mode::Detect(o) = options_for("app", &cfg.target["app"]).unwrap() else {
        panic!("expected detect mode")
    };
    assert_eq!(o.provider, Some(ProviderId::Laravel));
    assert_eq!(o.runtime.php.as_deref(), Some("8.4"));
    assert_eq!(o.key_prefix, "target.app");

    let alias = parse("[target.app.php]\nport = 9000\n").unwrap();
    let Mode::Detect(o) = options_for("app", &alias.target["app"]).unwrap() else {
        panic!()
    };
    assert_eq!(o.port, Some(9000));

    let none = parse("[target.app]\nprovider = \"none\"\n").unwrap();
    assert!(matches!(
        options_for("app", &none.target["app"]).unwrap(),
        Mode::Explicit
    ));

    for (toml, key) in [
        ("[target.app]\nprovider = \"cobol\"\n", "target.app.provider"),
        ("[target.app]\nport = 0\n", "target.app.port"),
        ("[target.app.base]\ncatalog = [\"os-min\"]\n", "target.app.base"),
        ("[target.app]\nexport = [\"/out\"]\n", "target.app.export"),
        ("[target.app]\nroot = \"../x\"\n", "target.app.root"),
        ("[target.App]\n", "target.App"),
        ("[target.app.steps.Bad]\nrun = \"x\"\n", "target.app.steps.Bad"),
    ] {
        let cfg = parse(toml).unwrap();
        let (name, tc) = targets(&cfg).remove(0);
        match options_for(&name, &tc) {
            Err(ConfigError::Invalid { key: k, .. }) => assert_eq!(k, key, "{toml}"),
            other => panic!("{toml}: expected Invalid({key}), got {other:?}"),
        }
    }
}
