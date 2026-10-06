use std::fmt;

use thiserror::Error;

use crate::repo::RepoError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderId {
    Php,
    Laravel,
    Node,
    Bun,
    Deno,
    Static,
    Python,
    Ruby,
    Go,
    Rust,
}

impl ProviderId {
    pub const ALL: [ProviderId; 10] = [
        ProviderId::Php,
        ProviderId::Laravel,
        ProviderId::Node,
        ProviderId::Bun,
        ProviderId::Deno,
        ProviderId::Static,
        ProviderId::Python,
        ProviderId::Ruby,
        ProviderId::Go,
        ProviderId::Rust,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Php => "php",
            ProviderId::Laravel => "laravel",
            ProviderId::Node => "node",
            ProviderId::Bun => "bun",
            ProviderId::Deno => "deno",
            ProviderId::Static => "static",
            ProviderId::Python => "python",
            ProviderId::Ruby => "ruby",
            ProviderId::Go => "go",
            ProviderId::Rust => "rust",
        }
    }

    pub fn parse(s: &str) -> Option<ProviderId> {
        ProviderId::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Error)]
pub enum DetectError {
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error("{}", no_provider(.candidates))]
    NoProvider { candidates: Vec<String> },
    #[error(
        "{} all claim the build root; choose one with `provider = \"…\"` under [target.<name>] in potter.toml, or split the repo into [target.*] entries",
        join(.providers)
    )]
    Ambiguous { providers: Vec<ProviderId> },
    #[error("provider `{0}` is set in potter.toml but does not apply to this directory")]
    ForcedNotApplicable(ProviderId),
    #[error("{provider}: {file} is required: {hint}")]
    MissingFile {
        provider: ProviderId,
        file: String,
        hint: String,
    },
    #[error("{file}: {reason}")]
    Parse { file: String, reason: String },
    #[error("invalid {what} constraint `{constraint}` ({source_desc}): {reason}")]
    BadConstraint {
        what: &'static str,
        constraint: String,
        source_desc: String,
        reason: String,
    },
    #[error("no available {runtime} version satisfies {source_desc} (available: {available})")]
    Unsatisfiable {
        runtime: &'static str,
        source_desc: String,
        available: String,
    },
    #[error("{provider}: no web process found: {hint}")]
    NoProcess { provider: ProviderId, hint: String },
    #[error("{provider}: {message}; set `{key}` in potter.toml")]
    NeedsConfig {
        provider: ProviderId,
        message: String,
        key: String,
    },
}

fn join(ids: &[ProviderId]) -> String {
    ids.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ")
}

fn no_provider(candidates: &[String]) -> String {
    let mut s = String::from("no provider recognises the build root");
    if !candidates.is_empty() {
        let dirs: Vec<String> = candidates.iter().map(|c| format!("{c}/")).collect();
        s.push_str(&format!(
            "; found manifests in {}. Declare each as a target in potter.toml, e.g. [target.{}] root = \"{}\"",
            dirs.join(", "),
            candidates[0].replace('/', "-"),
            candidates[0]
        ));
    }
    s
}
