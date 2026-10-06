//! potter's build plan IR (spec §5).

mod error;
mod ir;
#[cfg(test)]
mod testing;
mod validate;

pub use error::{Issue, PlanError};
pub use ir::*;
pub use validate::{is_name, is_relative_path, validate};
