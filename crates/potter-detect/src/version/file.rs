//! Single-version files: `.php-version`, `.nvmrc`, `.node-version`, `.bun-version`, `.dvmrc`.

use super::{ConstraintError, VersionSet, parse_partial};

/// Returns the version set and whether only LTS lines qualify (`lts/*`).
pub fn parse_version_file(content: &str) -> Result<(VersionSet, bool), ConstraintError> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let line = content
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .find(|l| !l.is_empty())
        .ok_or_else(|| ConstraintError("the version file is empty".into()))?;
    let lower = line.to_ascii_lowercase();
    match lower.as_str() {
        "lts/*" => Ok((VersionSet::any(), true)),
        "node" | "latest" | "current" | "stable" => Ok((VersionSet::any(), false)),
        l if l.starts_with("lts/") => Err(ConstraintError(format!(
            "`{line}` names an LTS codename; use a major version such as 22 instead"
        ))),
        _ => Ok((parse_partial(line)?.x_range()?, false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Version;

    #[test]
    fn version_files() {
        let (s, lts) = parse_version_file("8.3\n").unwrap();
        assert!(s.contains(Version::new(8, 3, 9)) && !s.contains(Version::new(8, 4, 0)) && !lts);
        let (s, _) = parse_version_file("v20.11.0\r\n").unwrap();
        assert!(s.contains(Version::new(20, 11, 0)) && !s.contains(Version::new(20, 11, 1)));
        let (s, _) = parse_version_file("\u{feff}22 # pinned for prod\n").unwrap();
        assert!(s.contains(Version::new(22, 5, 0)));
        let (_, lts) = parse_version_file("lts/*").unwrap();
        assert!(lts);
        let (s, lts) = parse_version_file("node").unwrap();
        assert!(s.contains(Version::new(24, 0, 0)) && !lts);
        let (s, _) = parse_version_file("\n\n# comment\n20\n").unwrap();
        assert!(s.contains(Version::new(20, 0, 0)));
    }

    #[test]
    fn version_file_errors() {
        assert!(parse_version_file("lts/iron").unwrap_err().0.contains("lts/iron"));
        assert!(parse_version_file("").is_err());
        assert!(parse_version_file("banana").is_err());
    }
}
