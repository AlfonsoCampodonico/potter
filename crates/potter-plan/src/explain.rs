//! Where each plan field came from, for `potter plan --explain` (spec §6.2).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Detected { evidence: Vec<String> },
    Config { key: String },
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// A dotted field path, e.g. `runtime.php` or `steps.assets.command`.
    pub field: String,
    pub value: String,
    pub origin: Origin,
}

impl Decision {
    pub fn detected<I, S>(field: &str, value: impl Into<String>, evidence: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Decision {
            field: field.to_string(),
            value: value.into(),
            origin: Origin::Detected {
                evidence: evidence.into_iter().map(Into::into).collect(),
            },
        }
    }

    pub fn config(field: &str, value: impl Into<String>, key: impl Into<String>) -> Self {
        Decision {
            field: field.to_string(),
            value: value.into(),
            origin: Origin::Config { key: key.into() },
        }
    }

    pub fn default(field: &str, value: impl Into<String>) -> Self {
        Decision {
            field: field.to_string(),
            value: value.into(),
            origin: Origin::Default,
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Detected { evidence } => write!(f, "detected: {}", evidence.join("; ")),
            Origin::Config { key } => write!(f, "potter.toml {key}"),
            Origin::Default => f.write_str("default"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_display() {
        assert_eq!(
            Decision::detected("runtime.php", "8.4", ["composer.json require.php = ^8.2"])
                .origin
                .to_string(),
            "detected: composer.json require.php = ^8.2"
        );
        assert_eq!(
            Decision::config("runtime.php", "8.3", "target.app.runtime.php")
                .origin
                .to_string(),
            "potter.toml target.app.runtime.php"
        );
        assert_eq!(Decision::default("runtime.node", "22").origin.to_string(), "default");
    }
}
