//! potter's build engine (spec §9): inputs layers, OCI bases, the namespace executor,
//! sealing, export and image assembly.
#![forbid(unsafe_code)]

pub mod affected;
pub mod assemble;
pub mod base;
pub mod chain;
mod error;
pub mod graph;
pub mod ingest;
pub mod inputs;
pub mod merge;
pub mod ns;
pub mod publish;
pub mod remote;
pub mod sandboxed;
pub mod seal;
pub mod stopgap;
pub mod tarball;
#[cfg(test)]
mod testing;
pub mod unpack;

pub use error::BuildError;
