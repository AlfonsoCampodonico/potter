//! The plan IR (spec §5.1). Every frontend lowers onto these types.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The schema identifier every plan carries.
pub const SCHEMA: &str = "potter.plan/v1";

/// Debian snapshot used by `Apt` steps unless `[apt] snapshot` is set.
pub const DEFAULT_APT_SNAPSHOT: &str = "20261001T000000Z";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: String,
    pub targets: Vec<Target>,
}

impl Plan {
    pub fn new(targets: Vec<Target>) -> Self {
        Plan {
            schema: SCHEMA.to_string(),
            targets,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub name: String,
    /// Relative to the repository root; "." is the root itself.
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// The provider that produced this target, or "none" for an explicit plan.
    pub provider: String,
    pub build: Chain,
    pub image: Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    pub base: Base,
    #[serde(default)]
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Base {
    Catalog {
        layers: Vec<CatalogRef>,
    },
    Oci {
        reference: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
    },
    Stage {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl CatalogRef {
    pub fn new(name: &str, version: Option<&str>) -> Self {
        CatalogRef {
            name: name.to_string(),
            version: version.map(str::to_string),
        }
    }
}

impl fmt::Display for CatalogRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(v) => write!(f, "{}@{}", self.name, v),
            None => f.write_str(&self.name),
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// A host-built layer of source files (spec §9.2).
    Inputs {
        name: String,
        include: Vec<String>,
        #[serde(default)]
        exclude: Vec<String>,
        dest: String,
        owner: String,
    },
    Run {
        name: String,
        command: Command,
        #[serde(default)]
        env: BTreeMap<String, String>,
        workdir: String,
        user: String,
        #[serde(default = "yes")]
        network: bool,
        #[serde(default)]
        cache_mounts: Vec<CacheMount>,
        #[serde(default)]
        secrets: Vec<SecretRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u64>,
    },
    CopyFrom {
        name: String,
        from: CopySource,
        paths: Vec<String>,
        dest: String,
        owner: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
    },
    Apt {
        name: String,
        packages: Vec<String>,
        snapshot: String,
    },
    Meta {
        name: String,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workdir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(default)]
        labels: BTreeMap<String, String>,
    },
}

impl Step {
    pub fn name(&self) -> &str {
        match self {
            Step::Inputs { name, .. }
            | Step::Run { name, .. }
            | Step::CopyFrom { name, .. }
            | Step::Apt { name, .. }
            | Step::Meta { name, .. } => name,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Step::Inputs { .. } => "inputs",
            Step::Run { .. } => "run",
            Step::CopyFrom { .. } => "copy_from",
            Step::Apt { .. } => "apt",
            Step::Meta { .. } => "meta",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Argv(Vec<String>),
    Shell(String),
}

impl Command {
    pub fn argv(args: &[&str]) -> Self {
        Command::Argv(args.iter().map(|s| s.to_string()).collect())
    }

    pub fn shell(script: &str) -> Self {
        Command::Shell(script.to_string())
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Argv(args) => f.write_str(&args.join(" ")),
            Command::Shell(s) => f.write_str(s),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CopySource {
    Stage(String),
    Target(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sharing {
    #[default]
    Exclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CacheMount {
    pub id: String,
    pub target: String,
    #[serde(default)]
    pub sharing: Sharing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub id: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub base: Base,
    /// Runtime system packages; only `Step::Apt` is allowed here (checked by `validate`).
    #[serde(default)]
    pub runtime_steps: Vec<Step>,
    #[serde(default)]
    pub export: Vec<Export>,
    #[serde(default)]
    pub files: Vec<GeneratedFile>,
    pub config: ImageConfig,
    #[serde(default)]
    pub processes: BTreeMap<String, Process>,
    #[serde(default)]
    pub services: BTreeMap<String, Vec<Service>>,
    /// The most erofs layers the image may have (3-128; absent: 10). Above it, the bottom
    /// layers are merged into one composite, the base's first (`docs/layer-budget.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_layers: Option<u32>,
}

impl Image {
    /// The layer budget in effect.
    pub fn layer_budget(&self) -> u32 {
        self.max_layers.unwrap_or(crate::budget::DEFAULT_MAX_LAYERS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub path: String,
    pub dest: String,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageConfig {
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub workdir: String,
    pub user: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_signal: Option<String>,
}

impl ImageConfig {
    pub fn new(workdir: &str, user: &str) -> Self {
        ImageConfig {
            env: BTreeMap::new(),
            workdir: workdir.to_string(),
            user: user.to_string(),
            labels: BTreeMap::new(),
            port: None,
            stop_signal: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl Process {
    pub fn new(command: &[&str]) -> Self {
        Process {
            command: command.iter().map(|s| s.to_string()).collect(),
            workdir: None,
            env: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub name: String,
    pub command: Vec<String>,
    pub ready: Ready,
    pub stop_signal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Ready {
    Unix(String),
    Tcp(u16),
    None,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sample_plan;

    #[test]
    fn plan_round_trips_through_json() {
        let plan = sample_plan();
        let json = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }

    #[test]
    fn steps_are_internally_tagged() {
        let plan = sample_plan();
        let v = serde_json::to_value(&plan).unwrap();
        assert_eq!(v["targets"][0]["build"]["steps"][0]["kind"], "inputs");
        assert_eq!(v["targets"][0]["build"]["steps"][1]["kind"], "run");
        assert_eq!(
            v["targets"][0]["build"]["steps"][1]["command"]["shell"],
            "composer install --no-dev"
        );
        assert_eq!(v["targets"][0]["build"]["base"]["kind"], "catalog");
        assert_eq!(
            v["targets"][0]["image"]["services"]["web"][0]["ready"]["unix"],
            "/run/php-fpm.sock"
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut v = serde_json::to_value(sample_plan()).unwrap();
        v["targets"][0]["build"]["steps"][1]["surprise"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Plan>(v).is_err());
        let mut v = serde_json::to_value(sample_plan()).unwrap();
        v["targets"][0]["surprise"] = serde_json::json!(1);
        assert!(serde_json::from_value::<Plan>(v).is_err());
    }

    #[test]
    fn ready_none_is_a_bare_string() {
        assert_eq!(serde_json::to_value(Ready::None).unwrap(), serde_json::json!("none"));
    }

    #[test]
    fn catalog_ref_display() {
        assert_eq!(CatalogRef::new("php", Some("8.4")).to_string(), "php@8.4");
        assert_eq!(CatalogRef::new("nginx", None).to_string(), "nginx");
    }

    #[test]
    fn command_display() {
        assert_eq!(
            Command::argv(&["php", "artisan", "horizon"]).to_string(),
            "php artisan horizon"
        );
        assert_eq!(Command::shell("a && b").to_string(), "a && b");
    }
}
