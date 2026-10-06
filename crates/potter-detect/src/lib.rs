//! potter's detectors (spec §6.1). Detection is static parsing only: nothing in the repository is
//! executed (P9).

#![allow(dead_code)] // Removed in Task 8 once every module is wired in.

pub mod catalog;
mod error;
pub mod repo;
pub mod version;

pub use error::{DetectError, ProviderId};
