//! Shared fixtures for unit tests.

use std::collections::BTreeMap;

use crate::ir::*;

pub(crate) fn sample_plan() -> Plan {
    let run = Step::Run {
        name: "deps".into(),
        command: Command::shell("composer install --no-dev"),
        env: BTreeMap::from([("COMPOSER_CACHE_DIR".into(), "/cache/composer".into())]),
        workdir: "/app".into(),
        user: "root".into(),
        network: true,
        cache_mounts: vec![CacheMount {
            id: "composer".into(),
            target: "/cache/composer".into(),
            sharing: Sharing::Exclusive,
        }],
        secrets: vec![],
        timeout_secs: None,
    };
    let inputs = Step::Inputs {
        name: "deps-inputs".into(),
        include: vec!["composer.json".into(), "composer.lock".into()],
        exclude: vec![],
        dest: "/app".into(),
        owner: "0:0".into(),
    };
    let mut config = ImageConfig::new("/app", "root");
    config.port = Some(8080);
    Plan::new(vec![Target {
        name: "app".into(),
        root: ".".into(),
        platform: None,
        provider: "php".into(),
        build: Chain {
            base: Base::Catalog {
                layers: vec![CatalogRef::new("os-min", None), CatalogRef::new("php", Some("8.4"))],
            },
            steps: vec![inputs, run],
        },
        image: Image {
            base: Base::Catalog {
                layers: vec![CatalogRef::new("os-min", None), CatalogRef::new("php-fpm", Some("8.4"))],
            },
            runtime_steps: vec![],
            export: vec![Export {
                path: "/app".into(),
                dest: "/app".into(),
                exclude: vec!["node_modules".into()],
            }],
            files: vec![GeneratedFile {
                path: "/etc/potter/nginx.conf".into(),
                content: "daemon off;\n".into(),
                mode: 0o644,
            }],
            config,
            processes: BTreeMap::from([("web".into(), Process::new(&["nginx", "-c", "/etc/potter/nginx.conf"]))]),
            services: BTreeMap::from([(
                "web".into(),
                vec![Service {
                    name: "php-fpm".into(),
                    command: vec!["php-fpm".into(), "--nodaemonize".into()],
                    ready: Ready::Unix("/run/php-fpm.sock".into()),
                    stop_signal: "SIGQUIT".into(),
                }],
            )]),
            max_layers: None,
        },
    }])
}
