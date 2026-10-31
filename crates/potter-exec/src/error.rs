//! Build failures and their exit codes (spec §11, §12).

/// Why a build failed.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("{path}: a symlink to {target} leaves the target's root, and potter never follows one out")]
    SymlinkEscape { path: String, target: String },
    #[error("{path}: sockets, FIFOs and devices cannot be build inputs")]
    SpecialFile { path: String },
    #[error("{context}: {source}")]
    Io { context: String, source: std::io::Error },
    #[error("{what} is not supported yet: it ships in {milestone}")]
    Unsupported { what: String, milestone: &'static str },
    #[error("{0}")]
    Plan(String),
    #[error("step {step} failed ({status})")]
    StepFailed {
        step: String,
        status: String,
        /// The last lines of the step's output.
        tail: Vec<String>,
    },
    #[error("sandbox violation: {0}")]
    Hostile(String),
    /// The step's output cannot become a layer: too large, or names a layer cannot hold.
    #[error("{0}")]
    StepOutput(String),
    #[error("{0}")]
    Infra(String),
    /// A rebase that kiln's gates refused: the image needs a rebuild (spec §16).
    #[error("the rebase is refused, rebuild the image instead: {}", .0.join("; "))]
    RebaseRefused(Vec<String>),
}

impl BuildError {
    /// The process exit code (spec §11): 1 a step failed or a rebase was refused, 2 the plan or a usage error, 3 a
    /// sandbox or protocol violation, 4 infrastructure.
    pub fn exit_code(&self) -> u8 {
        match self {
            BuildError::StepFailed { .. } | BuildError::StepOutput(_) | BuildError::RebaseRefused(_) => 1,
            BuildError::SymlinkEscape { .. }
            | BuildError::SpecialFile { .. }
            | BuildError::Unsupported { .. }
            | BuildError::Plan(_) => 2,
            BuildError::Hostile(_) => 3,
            BuildError::Io { .. } | BuildError::Infra(_) => 4,
        }
    }
}

/// An I/O error with what was being done.
pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> BuildError {
    let context = context.into();
    move |source| BuildError::Io { context, source }
}
