//! potter's build engine (spec §9): inputs layers, OCI bases, the namespace executor,
//! sealing, export and image assembly.
#![forbid(unsafe_code)]

mod error;
pub mod inputs;
pub mod tarball;

pub use error::BuildError;
