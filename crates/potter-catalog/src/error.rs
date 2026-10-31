use thiserror::Error;

/// Catalog failures, grouped by who is at fault and what exit code that earns.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CatalogError {
    /// The catalog's bytes are malformed or fail verification.
    #[error("catalog is hostile: {0}")]
    Hostile(String),
    /// The catalog could not be reached or stored.
    #[error("catalog infrastructure error: {0}")]
    Infra(String),
    /// The catalog is sound but cannot satisfy the request.
    #[error("catalog cannot satisfy the request: {0}")]
    Unsatisfied(String),
    /// The request itself is wrong.
    #[error("{0}")]
    Plan(String),
}

impl CatalogError {
    pub fn exit_code(&self) -> u8 {
        match self {
            CatalogError::Hostile(_) => 3,
            CatalogError::Infra(_) => 4,
            CatalogError::Unsatisfied(_) | CatalogError::Plan(_) => 2,
        }
    }
}
