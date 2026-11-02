//! potter's signed runtime catalog (P3).

mod error;
mod fetch;
mod index;
mod location;
mod resolve;
mod sign;

pub use error::CatalogError;
pub use fetch::{
    INDEX_TYPE, KEY, LAYER_TYPE, SIGNATURE, Verified, exists, fetch_layer, load, load_trusting, push, write_layout,
};
pub use index::{Arch, Entry, Index, Kind, MAX_ENTRIES, MAX_INDEX_BYTES, SCHEMA};
pub use location::{DEFAULT_CATALOG, INDEX_TAG, Location};
pub use resolve::{PHP_ONLY, Resolution, is_php_only, resolve};
pub use sign::{
    SigningKey, TrustedKey, compiled_keys, generate_pem, key_id, sign, signing_key_from_pem, trusted_keys, verify,
};
