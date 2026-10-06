//! The potter.toml schema (spec §6.2). Unknown keys are errors.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use indexmap::IndexMap;
use serde::Deserialize;

use crate::error::ConfigError;

pub const FILE_NAME: &str = "potter.toml";
pub const MAX_BYTES: u64 = 1024 * 1024;
/// Debian snapshot used by `Apt` steps unless `[apt] snapshot` is set.
pub const DEFAULT_APT_SNAPSHOT: &str = "20261001T000000Z";

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub target: IndexMap<String, TargetConfig>,
    #[serde(default)]
    pub apt: AptConfig,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    pub root: Option<String>,
    pub provider: Option<String>,
    pub port: Option<u16>,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub php: PhpConfig,
    #[serde(default)]
    pub node: NodeConfig,
    #[serde(default)]
    pub steps: IndexMap<String, StepConfig>,
    #[serde(default)]
    pub process: IndexMap<String, ProcessConfig>,
    #[serde(default)]
    pub build: CommandsConfig,
    #[serde(default)]
    pub deploy: CommandsConfig,
    /// Explicit plans only (`provider = "none"`).
    pub base: Option<BaseConfig>,
    /// Explicit plans only: absolute paths exported into the image. Defaults to `/app`.
    pub export: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub php: Option<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    pub node: Option<String>,
    pub bun: Option<String>,
    pub deno: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhpConfig {
    pub nginx_conf: Option<String>,
    pub fpm_pool: Option<String>,
    pub octane_server: Option<String>,
    /// Alias for `[target.x] port`.
    pub port: Option<u16>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub package_manager: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepConfig {
    pub run: Option<String>,
    pub inputs: Option<Vec<String>>,
    #[serde(default)]
    pub skip: bool,
    pub after: Option<String>,
    pub network: Option<bool>,
    /// Cache mount ids, mounted at `/cache/<id>`.
    pub cache: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessConfig {
    pub cmd: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandsConfig {
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseConfig {
    pub catalog: Option<Vec<String>>,
    pub oci: Option<String>,
    pub run_catalog: Option<Vec<String>>,
    pub run_oci: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AptConfig {
    #[serde(default)]
    pub build: Vec<String>,
    #[serde(default)]
    pub runtime: Vec<String>,
    pub snapshot: Option<String>,
}

pub fn parse(text: &str) -> Result<ConfigFile, ConfigError> {
    toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))
}

pub fn load(root: &Path) -> Result<ConfigFile, ConfigError> {
    let path = root.join(FILE_NAME);
    let io = |source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    };
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(ConfigFile::default()),
        Err(e) => return Err(io(e)),
    };
    if meta.file_type().is_symlink() {
        return Err(ConfigError::invalid(FILE_NAME, "must be a regular file, not a symlink"));
    }
    if meta.len() > MAX_BYTES {
        return Err(ConfigError::TooLarge(meta.len()));
    }
    parse(&fs::read_to_string(&path).map_err(io)?)
}

/// Declared targets, or a single implicit `app` target at the root.
pub fn targets(cfg: &ConfigFile) -> Vec<(String, TargetConfig)> {
    if cfg.target.is_empty() {
        vec![("app".to_string(), TargetConfig::default())]
    } else {
        cfg.target.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}
