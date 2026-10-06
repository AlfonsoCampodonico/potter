use super::{Bound, ConstraintError, Version, VersionSet};

/// A version with optional trailing components. `wildcard` records an explicit `x`, `X` or `*`,
/// which composer treats differently from an omitted component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Partial {
    pub major: Option<u64>,
    pub minor: Option<u64>,
    pub patch: Option<u64>,
    pub wildcard: bool,
}

pub(crate) fn bump(n: u64) -> Result<u64, ConstraintError> {
    n.checked_add(1)
        .ok_or_else(|| ConstraintError("version number too large".into()))
}

pub(crate) fn parse_partial(input: &str) -> Result<Partial, ConstraintError> {
    let bad = || ConstraintError(format!("invalid version `{input}`"));
    let mut s = input.trim();
    s = s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s);
    // Build metadata, then prerelease/stability suffixes, are ignored.
    if let Some(i) = s.find('+') {
        s = &s[..i];
    }
    if let Some(i) = s.find('-') {
        s = &s[..i];
    }
    if s.is_empty() {
        return Err(bad());
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() > 4 {
        return Err(bad());
    }
    let mut nums: [Option<u64>; 3] = [None; 3];
    let mut wildcard = false;
    for (i, p) in parts.iter().enumerate().take(3) {
        if matches!(*p, "x" | "X" | "*") {
            wildcard = true;
            continue;
        }
        if wildcard || p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        nums[i] = Some(p.parse().map_err(|_| bad())?);
    }
    if let Some(fourth) = parts.get(3)
        && (fourth.is_empty() || !(fourth.bytes().all(|b| b.is_ascii_digit()) || matches!(*fourth, "x" | "X" | "*")))
    {
        return Err(bad());
    }
    Ok(Partial {
        major: nums[0],
        minor: nums[1],
        patch: nums[2],
        wildcard,
    })
}

impl Partial {
    pub fn floor(&self) -> Version {
        Version::new(
            self.major.unwrap_or(0),
            self.minor.unwrap_or(0),
            self.patch.unwrap_or(0),
        )
    }

    /// The first version above everything this partial names: `1` → 2.0.0, `1.2` → 1.3.0, `1.2.3` → 1.2.4.
    pub fn upper_exclusive(&self) -> Result<Option<Version>, ConstraintError> {
        Ok(match (self.major, self.minor, self.patch) {
            (None, _, _) => None,
            (Some(m), None, _) => Some(Version::new(bump(m)?, 0, 0)),
            (Some(m), Some(n), None) => Some(Version::new(m, bump(n)?, 0)),
            (Some(m), Some(n), Some(p)) => Some(Version::new(m, n, bump(p)?)),
        })
    }

    /// npm X-range semantics: missing or wildcard components match anything.
    pub fn x_range(&self) -> Result<VersionSet, ConstraintError> {
        match self.upper_exclusive()? {
            None => Ok(VersionSet::any()),
            Some(hi) => Ok(VersionSet::interval(
                Bound::Inclusive(self.floor()),
                Bound::Exclusive(hi),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partials() {
        assert_eq!(
            parse_partial("8").unwrap(),
            Partial {
                major: Some(8),
                minor: None,
                patch: None,
                wildcard: false
            }
        );
        assert_eq!(
            parse_partial("v1.2.3").unwrap(),
            Partial {
                major: Some(1),
                minor: Some(2),
                patch: Some(3),
                wildcard: false
            }
        );
        assert_eq!(
            parse_partial("1.2.x").unwrap(),
            Partial {
                major: Some(1),
                minor: Some(2),
                patch: None,
                wildcard: true
            }
        );
        assert_eq!(
            parse_partial("*").unwrap(),
            Partial {
                major: None,
                minor: None,
                patch: None,
                wildcard: true
            }
        );
        assert_eq!(parse_partial("1.2.3-beta.1+build").unwrap().patch, Some(3));
        assert_eq!(parse_partial("1.2.3.4").unwrap().patch, Some(3));
        assert_eq!(
            parse_partial("1.0.x-dev").unwrap(),
            Partial {
                major: Some(1),
                minor: Some(0),
                patch: None,
                wildcard: true
            }
        );
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "a.b", "1..2", "1.2.3.4.5", "-1", "1.2a", "99999999999999999999"] {
            assert!(parse_partial(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn x_ranges() {
        let r = parse_partial("1.2").unwrap().x_range().unwrap();
        assert!(r.contains(Version::new(1, 2, 9)));
        assert!(!r.contains(Version::new(1, 3, 0)));
        let r = parse_partial("1").unwrap().x_range().unwrap();
        assert!(r.contains(Version::new(1, 9, 0)));
        assert!(!r.contains(Version::new(2, 0, 0)));
        assert!(
            parse_partial("*")
                .unwrap()
                .x_range()
                .unwrap()
                .contains(Version::new(0, 0, 0))
        );
        assert!(bump(u64::MAX).is_err());
    }
}
