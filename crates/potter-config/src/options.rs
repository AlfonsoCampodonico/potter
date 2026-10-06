//! potter.toml → detection options (spec §6.2).

use potter_detect::{DetectOptions, NodeOptions, PhpOptions, ProviderId, RuntimeOverrides};
use potter_plan::{is_name, is_relative_path};

use crate::error::ConfigError;
use crate::file::TargetConfig;

#[derive(Debug)]
pub enum Mode {
    Detect(Box<DetectOptions>),
    /// `provider = "none"`: the plan comes entirely from potter.toml.
    Explicit,
}

pub fn options_for(name: &str, tc: &TargetConfig) -> Result<Mode, ConfigError> {
    let prefix = format!("target.{name}");
    if !is_name(name) {
        return Err(ConfigError::invalid(&prefix, "target names match [a-z0-9._-]{1,64}"));
    }
    if let Some(root) = &tc.root
        && !is_relative_path(root)
    {
        return Err(ConfigError::invalid(
            format!("{prefix}.root"),
            "must be a relative path inside the repository",
        ));
    }
    for step in tc.steps.keys() {
        if !is_name(step) {
            return Err(ConfigError::invalid(
                format!("{prefix}.steps.{step}"),
                "step names match [a-z0-9._-]{1,64}",
            ));
        }
    }
    for process in tc.process.keys() {
        if !is_name(process) {
            return Err(ConfigError::invalid(
                format!("{prefix}.process.{process}"),
                "process names match [a-z0-9._-]{1,64}",
            ));
        }
    }
    let port = tc.port.or(tc.php.port);
    if port == Some(0) {
        let key = if tc.port.is_some() {
            format!("{prefix}.port")
        } else {
            format!("{prefix}.php.port")
        };
        return Err(ConfigError::invalid(key, "port must be 1-65535"));
    }
    let provider = match tc.provider.as_deref() {
        Some("none") => return Ok(Mode::Explicit),
        Some(p) => Some(ProviderId::parse(p).ok_or_else(|| {
            ConfigError::invalid(
                format!("{prefix}.provider"),
                format!("unknown provider `{p}` (php, laravel, node, bun, deno, static or none)"),
            )
        })?),
        None => None,
    };
    if tc.base.is_some() {
        return Err(ConfigError::invalid(
            format!("{prefix}.base"),
            "only used with provider = \"none\"",
        ));
    }
    if tc.export.is_some() {
        return Err(ConfigError::invalid(
            format!("{prefix}.export"),
            "only used with provider = \"none\"",
        ));
    }
    Ok(Mode::Detect(Box::new(DetectOptions {
        provider,
        port,
        runtime: RuntimeOverrides {
            php: tc.runtime.php.clone(),
            extensions: tc.runtime.extensions.clone(),
            node: tc.runtime.node.clone(),
            bun: tc.runtime.bun.clone(),
            deno: tc.runtime.deno.clone(),
        },
        php: PhpOptions {
            octane_server: tc.php.octane_server.clone(),
            nginx_conf: tc.php.nginx_conf.clone(),
            fpm_pool: tc.php.fpm_pool.clone(),
        },
        node: NodeOptions {
            package_manager: tc.node.package_manager.clone(),
        },
        key_prefix: prefix,
    })))
}
