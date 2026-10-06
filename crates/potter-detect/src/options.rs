//! Inputs to detection that come from potter.toml (spec §6.2).

use crate::ProviderId;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectOptions {
    /// `provider = "…"`: skip selection and use this provider.
    pub provider: Option<ProviderId>,
    /// `[target.x] port` (or the `[target.x.php] port` alias).
    pub port: Option<u16>,
    pub runtime: RuntimeOverrides,
    pub php: PhpOptions,
    pub node: NodeOptions,
    pub python: PythonOptions,
    /// `[target.x.process.web] cmd`: when set, providers don't need to infer a web process.
    pub web_cmd: Option<Vec<String>>,
    /// The potter.toml key prefix used in decisions and hints, e.g. `target.api`. Empty means `target.app`.
    pub key_prefix: String,
}

impl DetectOptions {
    pub fn key(&self, rest: &str) -> String {
        let prefix = if self.key_prefix.is_empty() {
            "target.app"
        } else {
            &self.key_prefix
        };
        format!("{prefix}.{rest}")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeOverrides {
    pub php: Option<String>,
    pub extensions: Vec<String>,
    pub node: Option<String>,
    pub bun: Option<String>,
    pub deno: Option<String>,
    pub python: Option<String>,
    pub ruby: Option<String>,
    pub go: Option<String>,
    pub rust: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhpOptions {
    pub octane_server: Option<String>,
    pub nginx_conf: Option<String>,
    pub fpm_pool: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeOptions {
    pub package_manager: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PythonOptions {
    pub package_manager: Option<String>,
    /// `[target.x.python.django] collectstatic`; `None` means enabled.
    pub collectstatic: Option<bool>,
}
