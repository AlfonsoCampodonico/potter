//! potter's signed runtime catalog (P3).

mod error;
mod index;

pub use error::CatalogError;
pub use index::{Arch, Entry, Index, Kind, MAX_ENTRIES, MAX_INDEX_BYTES, SCHEMA};
