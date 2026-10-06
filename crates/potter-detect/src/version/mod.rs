//! Version constraints for runtime selection. Prerelease and build metadata are ignored: potter
//! selects release lines, never prereleases.

mod partial;
mod set;

#[allow(unused_imports)] // Used by the parsers from Task 5.
pub(crate) use partial::{Partial, bump, parse_partial};
pub use set::{Bound, Interval, Version, VersionSet};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConstraintError(pub String);
