//! potter's build engine (spec §9): inputs layers, OCI bases, the namespace executor,
//! sealing, export and image assembly.
#![forbid(unsafe_code)]

pub mod base;
mod error;
pub mod inputs;
pub mod merge;
pub mod tarball;
#[cfg(test)]
mod testing;

pub use error::BuildError;
