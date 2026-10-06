mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions, PhpOptions};

#[test]
fn laravel_with_two_lockfiles_uses_npm() {
    let d = detect("laravel");
    let t = &d.target;
    assert_eq!(t.provider, "laravel");
    assert_eq!(decision(&d, "provider").value, "laravel");
    assert_eq!(decision(&d, "runtime.php").value, "8.4");
    assert_eq!(
        step_names(t),
        [
            "deps-inputs",
            "deps",
            "node-deps-inputs",
            "node-deps",
            "source",
            "autoload",
            "discover",
            "assets"
        ]
    );
    assert_eq!(command(t, "node-deps"), "npm ci --no-audit --no-fund");
    let pm = decision(&d, "node.package_manager");
    assert_eq!(pm.value, "npm");
    assert!(pm.origin.to_string().contains("several lockfiles"), "{}", pm.origin);
    assert_eq!(command(t, "assets"), "npm run build");
    assert!(command(t, "autoload").ends_with("--no-scripts"));
    assert_eq!(command(t, "discover"), "php artisan package:discover --ansi");
    let build = layers(&t.build.base);
    assert_eq!(build.last().unwrap(), "node@22");
    for ext in [
        "php-ext-bcmath@8.4",
        "php-ext-redis@8.4",
        "php-ext-zip@8.4",
        "php-ext-pcntl@8.4",
    ] {
        assert!(build.contains(&ext.to_string()), "{ext} missing from {build:?}");
    }
    assert!(
        !layers(&t.image.base).iter().any(|l| l.starts_with("node")),
        "node must not ship"
    );
    assert_eq!(process(t, "web"), "nginx -c /etc/potter/nginx.conf");
    assert_eq!(process(t, "worker"), "php artisan queue:work");
    assert_eq!(process(t, "scheduler"), "php artisan schedule:work");
    assert_eq!(t.image.services["web"][0].name, "php-fpm");
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn octane_swoole() {
    let t = detect("laravel-octane").target;
    let run = layers(&t.image.base);
    assert!(run.contains(&"php-ext-swoole@8.4".to_string()), "{run:?}");
    assert!(!run.iter().any(|l| l == "nginx" || l.starts_with("php-fpm")), "{run:?}");
    assert_eq!(
        process(&t, "web"),
        "php artisan octane:start --server=swoole --host=0.0.0.0 --port=8080"
    );
    assert!(t.image.services.is_empty());
    assert!(t.image.files.is_empty());
    assert!(!t.image.config.env.contains_key("PHP_FPM_MAX_CHILDREN"));
    assert!(!step_names(&t).contains(&"assets"));
}

#[test]
fn octane_server_needs_config_when_unknown() {
    let r = temp_repo(&[
        (
            "composer.json",
            r#"{"require":{"laravel/framework":"^12.0","laravel/octane":"^2.6"}}"#,
        ),
        ("composer.lock", r#"{"packages":[]}"#),
        ("artisan", "<?php"),
    ]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(
        matches!(&e, DetectError::NeedsConfig { key, .. } if key == "target.app.php.octane_server"),
        "{e}"
    );
    let opts = DetectOptions {
        php: PhpOptions {
            octane_server: Some("frankenphp".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let t = detect_at(r.path(), &opts).unwrap().target;
    assert!(layers(&t.image.base).contains(&"frankenphp".to_string()));
    assert!(process(&t, "web").contains("--server=frankenphp"));
    let opts = DetectOptions {
        php: PhpOptions {
            octane_server: Some("apache".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(matches!(
        detect_at(r.path(), &opts),
        Err(DetectError::NeedsConfig { .. })
    ));
}

#[test]
fn roadrunner_from_packages() {
    let r = temp_repo(&[
        (
            "composer.json",
            r#"{"require":{"laravel/framework":"^12.0","laravel/octane":"^2.6","spiral/roadrunner-http":"^3.0"}}"#,
        ),
        ("composer.lock", r#"{"packages":[]}"#),
        ("artisan", "<?php"),
    ]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert!(layers(&t.image.base).contains(&"roadrunner".to_string()));
}

#[test]
fn horizon_and_pnpm_assets() {
    let t = detect("laravel-horizon-pnpm").target;
    assert_eq!(
        command(&t, "node-deps"),
        "corepack enable && pnpm install --frozen-lockfile"
    );
    assert_eq!(command(&t, "assets"), "pnpm run build");
    assert_eq!(process(&t, "worker"), "php artisan horizon");
}

#[test]
fn package_json_without_build_has_no_assets() {
    let r = temp_repo(&[
        ("composer.json", r#"{"require":{"laravel/framework":"^12.0"}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
        ("artisan", "<?php"),
        ("package.json", r#"{"scripts":{"dev":"vite"}}"#),
    ]);
    let d = detect_at(r.path(), &DetectOptions::default()).unwrap();
    assert!(!step_names(&d.target).contains(&"assets"));
    assert_eq!(decision(&d, "assets").value, "none");
}

#[test]
fn artisan_alone_is_not_laravel() {
    let r = temp_repo(&[
        ("composer.json", r#"{"require":{}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
        ("artisan", "<?php"),
    ]);
    assert_eq!(
        detect_at(r.path(), &DetectOptions::default()).unwrap().target.provider,
        "php"
    );
}
