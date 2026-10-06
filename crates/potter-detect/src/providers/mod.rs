//! Language providers (spec §6.1). Each provider adds itself to `all()` in its own task.

#[allow(dead_code)] // Every helper is used once all providers land (Task 13 removes this).
pub(crate) mod common;

use potter_plan::{Decision, Target};

use crate::options::DetectOptions;
use crate::repo::RepoView;
use crate::select::Claim;
use crate::{DetectError, ProviderId};

#[derive(Debug, Clone)]
pub struct Detection {
    pub target: Target,
    pub decisions: Vec<Decision>,
}

pub trait Provider: Sync {
    fn id(&self) -> ProviderId;
    /// Cheap: does this provider apply here, and how? Must not fail on files only `build` needs.
    fn claim(&self, repo: &RepoView, opts: &DetectOptions) -> Result<Option<Claim>, DetectError>;
    fn build(&self, repo: &RepoView, opts: &DetectOptions, target: &str) -> Result<Detection, DetectError>;
}

pub fn all() -> Vec<&'static dyn Provider> {
    vec![]
}

pub fn get(id: ProviderId) -> Option<&'static dyn Provider> {
    all().into_iter().find(|p| p.id() == id)
}
