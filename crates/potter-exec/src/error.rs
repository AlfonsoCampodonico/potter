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
    /// A refused rebase: the image needs a rebuild (spec §16).
    #[error("the rebase is refused, rebuild the image instead: {}", .reasons.join("; "))]
    RebaseRefused { reasons: Vec<String>, by: RefusedBy },
}

/// Which gates refused a rebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedBy {
    /// kiln's: the new base changes in a way the app could notice (exit 1).
    Kiln,
    /// The catalog's (design §8): a line, ABI or soname change, or a record the image lacks (exit 2).
    Catalog,
}

impl BuildError {
    /// The process exit code (spec §11): 1 a step failed or kiln refused a rebase, 2 the plan, a
    /// usage error or a catalog gate's refusal, 3 a sandbox or protocol violation, 4 infrastructure.
    pub fn exit_code(&self) -> u8 {
        match self {
            BuildError::StepFailed { .. }
            | BuildError::StepOutput(_)
            | BuildError::RebaseRefused {
                by: RefusedBy::Kiln, ..
            } => 1,
            BuildError::RebaseRefused {
                by: RefusedBy::Catalog, ..
            }
            | BuildError::SymlinkEscape { .. }
            | BuildError::SpecialFile { .. }
            | BuildError::Unsupported { .. }
            | BuildError::Plan(_) => 2,
            BuildError::Hostile(_) => 3,
            BuildError::Io { .. } | BuildError::Infra(_) => 4,
        }
    }
}

/// Catalog failures keep their exit codes: hostile 3, infrastructure 4, plan and unsatisfiable 2.
impl From<potter_catalog::CatalogError> for BuildError {
    fn from(e: potter_catalog::CatalogError) -> Self {
        use potter_catalog::CatalogError as C;
        let msg = e.to_string();
        match e {
            C::Hostile(_) => BuildError::Hostile(msg),
            C::Infra(_) => BuildError::Infra(msg),
            C::Unsatisfied(_) | C::Plan(_) => BuildError::Plan(msg),
        }
    }
}

/// An I/O error with what was being done.
pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> BuildError {
    let context = context.into();
    move |source| BuildError::Io { context, source }
}
