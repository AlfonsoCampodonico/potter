use std::io::ErrorKind;

use potter_detect::{DetectError, RepoError};
use potter_plan::PlanError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("potter.toml: {0}")]
    Parse(String),
    #[error("potter.toml is {0} bytes, over the 1 MiB limit")]
    TooLarge(u64),
    #[error("potter.toml: {key}: {reason}")]
    Invalid { key: String, reason: String },
    #[error("no target named `{name}` (targets: {available})")]
    UnknownTarget { name: String, available: String },
    #[error("target `{name}`: {source}")]
    InTarget { name: String, source: Box<ConfigError> },
    #[error(
        "target `{name}` (discovered automatically from {dir}/): {source}; declare the targets you want under [target.*], or set `discover = false`"
    )]
    InDiscoveredTarget {
        name: String,
        dir: String,
        source: Box<ConfigError>,
    },
    #[error(transparent)]
    Detect(#[from] DetectError),
    #[error(transparent)]
    Plan(#[from] PlanError),
}

impl ConfigError {
    pub(crate) fn invalid(key: impl Into<String>, reason: impl Into<String>) -> Self {
        ConfigError::Invalid {
            key: key.into(),
            reason: reason.into(),
        }
    }

    /// I/O failures other than "not found" are infrastructure problems (exit 4), not user errors.
    pub fn is_infrastructure(&self) -> bool {
        match self {
            ConfigError::Io { source, .. } => source.kind() != ErrorKind::NotFound,
            ConfigError::InTarget { source, .. } | ConfigError::InDiscoveredTarget { source, .. } => {
                source.is_infrastructure()
            }
            ConfigError::Detect(DetectError::Repo(RepoError::Io { source, .. })) => {
                source.kind() != ErrorKind::NotFound
            }
            _ => false,
        }
    }
}
