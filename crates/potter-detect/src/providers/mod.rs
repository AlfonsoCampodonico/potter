//! Language providers (spec §6.1).

pub(crate) mod bun;
pub(crate) mod common;
mod deno;
pub(crate) mod js;
mod laravel;
pub(crate) mod node;
mod php;
mod php_config;
mod python;
mod static_site;

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
    vec![
        &php::PhpProvider,
        &laravel::LaravelProvider,
        &node::NodeProvider,
        &bun::BunProvider,
        &deno::DenoProvider,
        &static_site::StaticProvider,
        &python::PythonProvider,
    ]
}

pub fn get(id: ProviderId) -> Option<&'static dyn Provider> {
    all().into_iter().find(|p| p.id() == id)
}
