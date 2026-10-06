//! potter's detectors (spec §6.1). Detection is static parsing only: nothing in the repository is
//! executed (P9).

pub mod catalog;
mod error;
mod jsonc;
mod options;
pub mod providers;
pub mod repo;
pub(crate) mod scan;
mod select;
pub mod version;

use potter_plan::Decision;

pub use error::{DetectError, ProviderId};
pub use options::{DetectOptions, NodeOptions, PhpOptions, RuntimeOverrides};
pub use providers::{Detection, Provider};
pub use repo::{RepoError, RepoView};
pub use select::{Claim, ClaimKind, select};

/// Every provider's claim on this build root, in `ProviderId` order.
pub fn claims(repo: &RepoView, opts: &DetectOptions) -> Result<Vec<Claim>, DetectError> {
    let mut out = Vec::new();
    for p in providers::all() {
        if let Some(c) = p.claim(repo, opts)? {
            out.push(c);
        }
    }
    out.sort_by_key(|c| c.provider);
    Ok(out)
}

pub fn detect(repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError> {
    let (id, decision) = match opts.provider {
        Some(id) => {
            let p = providers::get(id).ok_or(DetectError::ForcedNotApplicable(id))?;
            if p.claim(repo, opts)?.is_none() {
                return Err(DetectError::ForcedNotApplicable(id));
            }
            (id, Decision::config("provider", id.as_str(), opts.key("provider")))
        }
        None => {
            let all = claims(repo, opts)?;
            let id = select(&all, repo)?;
            let evidence = all
                .iter()
                .find(|c| c.provider == id)
                .map(|c| c.evidence.clone())
                .unwrap_or_default();
            (id, Decision::detected("provider", id.as_str(), evidence))
        }
    };
    let provider = providers::get(id).expect("selected providers are registered");
    let mut detection = provider.build(repo, opts, target)?;
    detection.decisions.insert(0, decision);
    Ok(detection)
}
