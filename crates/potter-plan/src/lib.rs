//! potter's build plan IR (spec §5).

pub mod budget;
mod canon;
mod error;
mod explain;
mod ir;
mod sanitize;
mod schema;
#[cfg(test)]
mod testing;
mod validate;

pub use canon::{Sha256Digest, canonical_json, hash_of, plan_hash, step_hash};
pub use error::{Issue, PlanError};
pub use explain::{Decision, Origin};
pub use ir::*;
pub use sanitize::sanitize;
pub use schema::json_schema;
pub use validate::{is_name, is_relative_path, validate};
