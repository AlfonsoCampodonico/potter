mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions, PhpOptions, RuntimeOverrides};
use potter_plan::{Origin, Ready};

#[test]
fn composer_project() {
    let d = detect("php-composer");
    let t = &d.target;
    assert_eq!(t.provider, "php");
    assert_eq!(
        layers(&t.build.base),
        ["os-min", "php@8.4", "php-ext-intl@8.4", "composer@2"]
    );
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "autoload"]);
    assert_eq!(includes(t, "deps-inputs"), ["composer.json", "composer.lock"]);
    assert!(command(t, "deps").starts_with("composer install --no-dev --no-scripts --no-autoloader"));
    assert_eq!(
        layers(&t.image.base),
        ["os-min", "php@8.4", "php-fpm@8.4", "php-ext-intl@8.4", "nginx"]
    );
    assert_eq!(process(t, "web"), "nginx -c /etc/potter/nginx.conf");
    let fpm = &t.image.services["web"][0];
    assert_eq!(fpm.name, "php-fpm");
    assert_eq!(fpm.ready, Ready::Unix("/run/php-fpm.sock".into()));
    assert_eq!(t.image.config.port, Some(8080));
    assert_eq!(t.image.config.stop_signal.as_deref(), Some("SIGQUIT"));
    let nginx = &t
        .image
        .files
        .iter()
        .find(|f| f.path == "/etc/potter/nginx.conf")
        .unwrap()
        .content;
    assert!(
        nginx.contains("listen 8080;")
            && nginx.contains("root /app/public;")
            && nginx.contains("unix:/run/php-fpm.sock")
    );
    let fpm_conf = &t
        .image
        .files
        .iter()
        .find(|f| f.path == "/etc/potter/php-fpm.conf")
        .unwrap()
        .content;
    assert!(fpm_conf.contains("pm.max_children = ${PHP_FPM_MAX_CHILDREN}") && fpm_conf.contains("clear_env = no"));
    assert_eq!(t.image.config.env["PHP_FPM_MAX_CHILDREN"], "16");
    assert_eq!(decision(&d, "runtime.php").value, "8.4");
    assert!(
        matches!(&decision(&d, "runtime.php").origin, Origin::Detected { evidence } if evidence[0] == "composer.json require.php = ^8.3")
    );
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn dev_only_extensions_and_builtins_are_ignored() {
    let d = detect("php-composer");
    let l = layers(&d.target.build.base);
    assert!(!l.iter().any(|x| x.contains("xdebug")), "{l:?}");
    assert!(!l.iter().any(|x| x.contains("json")), "{l:?}");
}

#[test]
fn detection_never_runs_composer_scripts() {
    let _ = detect("php-composer");
    assert!(!fixture("php-composer").join("POTTER_EXECUTED").exists());
}

#[test]
fn drupal() {
    let d = detect("drupal");
    let t = &d.target;
    assert_eq!(decision(&d, "runtime.php").value, "8.4");
    for ext in [
        "php-ext-gd@8.4",
        "php-ext-opcache@8.4",
        "php-ext-pdo_mysql@8.4",
        "php-ext-pdo_pgsql@8.4",
    ] {
        assert!(layers(&t.image.base).contains(&ext.to_string()), "{ext}");
    }
    assert_eq!(decision(&d, "php.docroot").value, "public");
    let nginx = &t.image.files[0].content;
    assert!(nginx.contains("root /app/public;"));
}

#[test]
fn missing_lockfile() {
    let r = temp_repo(&[("composer.json", r#"{"require":{"php":"^8.2"}}"#)]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(
        matches!(&e, DetectError::MissingFile { file, .. } if file == "composer.lock"),
        "{e}"
    );
}

#[test]
fn unsatisfiable_php() {
    let r = temp_repo(&[
        ("composer.json", r#"{"require":{"php":"^7.4"}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
    ]);
    let msg = detect_at(r.path(), &DetectOptions::default()).unwrap_err().to_string();
    assert_eq!(
        msg,
        "no available php version satisfies composer.json require.php = ^7.4 (available: 8.2, 8.3, 8.4)"
    );
}

#[test]
fn version_precedence() {
    let files = [
        (
            "composer.json",
            r#"{"require":{"php":"^8.2"},"config":{"platform":{"php":"8.3.0"}}}"#,
        ),
        ("composer.lock", r#"{"packages":[]}"#),
    ];
    let r = temp_repo(&files);
    assert_eq!(
        decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.php").value,
        "8.3"
    );
    let r = temp_repo(&[files[0], files[1], (".php-version", "8.2\n")]);
    assert_eq!(
        decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "runtime.php").value,
        "8.2"
    );
    let opts = DetectOptions {
        runtime: RuntimeOverrides {
            php: Some("8.4".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let d = detect_at(r.path(), &opts).unwrap();
    assert_eq!(decision(&d, "runtime.php").value, "8.4");
    assert_eq!(
        decision(&d, "runtime.php").origin,
        Origin::Config {
            key: "target.app.runtime.php".into()
        }
    );
}

#[test]
fn bad_constraint_names_its_source() {
    let r = temp_repo(&[
        ("composer.json", r#"{"require":{"php":"dev-master"}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
    ]);
    let msg = detect_at(r.path(), &DetectOptions::default()).unwrap_err().to_string();
    assert!(
        msg.contains("composer.json require.php") && msg.contains("dev-master"),
        "{msg}"
    );
}

#[test]
fn docroot_fallbacks_and_overrides() {
    let base = [
        ("composer.json", r#"{"require":{}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
    ];
    let r = temp_repo(&[base[0], base[1], ("web/index.php", "<?php")]);
    assert_eq!(
        decision(&detect_at(r.path(), &DetectOptions::default()).unwrap(), "php.docroot").value,
        "web"
    );
    let r = temp_repo(&base);
    let d = detect_at(r.path(), &DetectOptions::default()).unwrap();
    assert!(d.target.image.files[0].content.contains("root /app;"));
    let r = temp_repo(&[base[0], base[1], ("deploy/nginx.conf", "events {}")]);
    let opts = DetectOptions {
        php: PhpOptions {
            nginx_conf: Some("deploy/nginx.conf".into()),
            ..Default::default()
        },
        port: Some(9000),
        ..Default::default()
    };
    let d = detect_at(r.path(), &opts).unwrap();
    assert_eq!(process(&d.target, "web"), "nginx -c /app/deploy/nginx.conf");
    assert!(d.target.image.files.iter().all(|f| f.path != "/etc/potter/nginx.conf"));
    assert_eq!(d.target.image.config.port, Some(9000));
    let opts = DetectOptions {
        php: PhpOptions {
            nginx_conf: Some("missing.conf".into()),
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
fn hostile_docroot_is_refused() {
    let r = temp_repo(&[
        (
            "composer.json",
            r#"{"extra":{"drupal-scaffold":{"locations":{"web-root":"public; include /etc/passwd"}}}}"#,
        ),
        ("composer.lock", r#"{"packages":[]}"#),
    ]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::Parse { .. })
    ));
}

#[test]
fn config_extensions_are_added() {
    let r = temp_repo(&[
        ("composer.json", r#"{"require":{}}"#),
        ("composer.lock", r#"{"packages":[]}"#),
    ]);
    let opts = DetectOptions {
        runtime: RuntimeOverrides {
            extensions: vec!["imagick".into(), "ext-redis".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let l = layers(&detect_at(r.path(), &opts).unwrap().target.build.base);
    assert!(
        l.contains(&"php-ext-imagick@8.4".to_string()) && l.contains(&"php-ext-redis@8.4".to_string()),
        "{l:?}"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_manifest_names_the_file() {
    let outside = temp_repo(&[("composer.json", r#"{"require":{}}"#)]);
    let r = temp_repo(&[("composer.lock", r#"{"packages":[]}"#)]);
    std::os::unix::fs::symlink(outside.path().join("composer.json"), r.path().join("composer.json")).unwrap();
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(
        matches!(&e, DetectError::Repo(potter_detect::RepoError::Symlink(p)) if p == "composer.json"),
        "{e}"
    );
    let forced = DetectOptions {
        provider: Some(potter_detect::ProviderId::Php),
        ..Default::default()
    };
    assert!(matches!(detect_at(r.path(), &forced), Err(DetectError::Repo(_))));
}
