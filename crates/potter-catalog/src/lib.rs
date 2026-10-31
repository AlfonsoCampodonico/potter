//! potter's signed runtime catalog (P3).

mod error;
mod index;
mod sign;

pub use error::CatalogError;
pub use index::{Arch, Entry, Index, Kind, MAX_ENTRIES, MAX_INDEX_BYTES, SCHEMA};
pub use sign::{TrustedKey, compiled_keys, generate_pem, key_id, sign, trusted_keys, verify};
