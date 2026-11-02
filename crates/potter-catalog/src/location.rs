//! Where the catalog lives: a registry reference or a local OCI image layout.

use std::fmt;
use std::path::PathBuf;

use kiln_registry::Reference;

use crate::CatalogError;

/// The catalog potter reads when `POTTER_CATALOG` is unset.
pub const DEFAULT_CATALOG: &str = "ghcr.io/alfonsocampodonico/potter-catalog:index";
/// The tag (and layout ref name) of the current index.
pub const INDEX_TAG: &str = "index";
const LAYOUT_PREFIX: &str = "oci-layout:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Registry(Reference),
    Layout(PathBuf),
}

impl Location {
    /// `oci-layout:DIR`, or a registry reference; a reference without a tag or digest gets the
    /// tag `index`.
    pub fn parse(s: &str) -> Result<Location, CatalogError> {
        if let Some(dir) = s.strip_prefix(LAYOUT_PREFIX) {
            if dir.is_empty() {
                return Err(CatalogError::Plan(format!("{s:?}: name the layout directory")));
            }
            return Ok(Location::Layout(PathBuf::from(dir)));
        }
        // A tag follows the last `:` after the last `/`; without one (or a digest), use `index`.
        let last = &s[s.rfind('/').map_or(0, |i| i + 1)..];
        let full = if s.is_empty() || s.contains('@') || last.contains(':') {
            s.to_string()
        } else {
            format!("{s}:{INDEX_TAG}")
        };
        Reference::parse(&full)
            .map(Location::Registry)
            .map_err(|e| CatalogError::Plan(format!("catalog location: {e}")))
    }

    /// The catalog named by `POTTER_CATALOG`, or [`DEFAULT_CATALOG`].
    pub fn from_env() -> Result<Location, CatalogError> {
        let (loc, notes) = Location::from_env_with(std::env::var("POTTER_CATALOG").ok().as_deref())?;
        for note in notes {
            eprintln!("{note}");
        }
        Ok(loc)
    }

    /// The location and the `note:` lines the caller should print.
    pub(crate) fn from_env_with(var: Option<&str>) -> Result<(Location, Vec<String>), CatalogError> {
        match var.filter(|v| !v.is_empty()) {
            None => Ok((Location::parse(DEFAULT_CATALOG)?, Vec::new())),
            Some(v) => {
                let loc = Location::parse(v).map_err(|e| CatalogError::Plan(format!("POTTER_CATALOG: {e}")))?;
                let note = format!("note: using the catalog at {loc} from POTTER_CATALOG");
                Ok((loc, vec![note]))
            }
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Registry(r) => write!(f, "{r}"),
            Location::Layout(dir) => write!(f, "{LAYOUT_PREFIX}{}", dir.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_layouts_and_references() {
        assert_eq!(
            Location::parse("oci-layout:/tmp/cat").unwrap(),
            Location::Layout(PathBuf::from("/tmp/cat"))
        );
        let Location::Registry(r) = Location::parse("localhost:5000/acme/cat").unwrap() else {
            panic!("not a registry");
        };
        assert_eq!(r.tag(), Some(INDEX_TAG));
        assert_eq!(r.repository(), "acme/cat");
        let Location::Registry(r) = Location::parse("ghcr.io/acme/cat:other").unwrap() else {
            panic!("not a registry");
        };
        assert_eq!(r.tag(), Some("other"));
        assert_eq!(Location::parse(DEFAULT_CATALOG).unwrap().to_string(), DEFAULT_CATALOG);
        assert_eq!(
            Location::parse("oci-layout:/tmp/cat").unwrap().to_string(),
            "oci-layout:/tmp/cat"
        );
    }

    #[test]
    fn bad_locations_are_plan_errors() {
        for bad in ["", "oci-layout:", "Not A Reference", "ghcr.io/acme/cat:bad tag"] {
            let err = Location::parse(bad).unwrap_err();
            assert!(matches!(err, CatalogError::Plan(_)), "{bad:?}: {err:?}");
        }
    }

    #[test]
    fn env_override_is_noted() {
        let (loc, notes) = Location::from_env_with(None).unwrap();
        assert_eq!(loc.to_string(), DEFAULT_CATALOG);
        assert!(notes.is_empty());
        let (loc, notes) = Location::from_env_with(Some("")).unwrap();
        assert_eq!(loc.to_string(), DEFAULT_CATALOG);
        assert!(notes.is_empty());
        let (loc, notes) = Location::from_env_with(Some("oci-layout:/tmp/cat")).unwrap();
        assert_eq!(loc, Location::Layout(PathBuf::from("/tmp/cat")));
        assert_eq!(
            notes,
            vec!["note: using the catalog at oci-layout:/tmp/cat from POTTER_CATALOG".to_string()]
        );
        let err = Location::from_env_with(Some("Not A Reference")).unwrap_err();
        assert!(
            matches!(&err, CatalogError::Plan(m) if m.contains("POTTER_CATALOG")),
            "{err:?}"
        );
    }
}
