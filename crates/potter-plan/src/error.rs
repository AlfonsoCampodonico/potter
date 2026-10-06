use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Where the problem is, e.g. `targets[0].build.steps[2].name`.
    pub at: String,
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("plan is invalid:\n{}", format_issues(.0))]
    Invalid(Vec<Issue>),
    #[error("floating-point numbers are not allowed in canonical JSON")]
    Float,
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

fn format_issues(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(|i| format!("  {}: {}", i.at, i.reason))
        .collect::<Vec<_>>()
        .join("\n")
}
