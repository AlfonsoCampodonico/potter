//! Version constraints for runtime selection. Prerelease and build metadata are ignored: potter
//! selects release lines, never prereleases.

mod composer;
mod npm;
mod partial;
mod set;

pub use composer::parse_composer_constraint;
pub use npm::parse_npm_range;
pub(crate) use partial::{Partial, bump, parse_partial};
pub use set::{Bound, Interval, Version, VersionSet};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConstraintError(pub String);
