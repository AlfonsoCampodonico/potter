//! PEP 440 specifiers (Python), RubyGems requirements, and the leading release number shared by
//! Go, Rust and Ruby version strings.

use super::{Bound, ConstraintError, Version, VersionSet, bump, npm_clause, parse_partial};

/// The leading `[0-9.*]` run: `3.12.0rc1` → `3.12.0`, `1.26.4` → `1.26.4`, `3.3.6p108` → `3.3.6`.
pub fn release_prefix(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    let end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '*'))
        .unwrap_or(s.len());
    s[..end].trim_end_matches('.')
}

/// The set of versions at or above `s` (Go's `go` directive, Cargo's `rust-version`).
pub fn at_least(s: &str) -> Result<VersionSet, ConstraintError> {
    let p = parse_partial(release_prefix(s))?;
    Ok(VersionSet::interval(Bound::Inclusive(p.floor()), Bound::Unbounded))
}

/// PEP 440 version specifiers, e.g. `>=3.11,<3.14` or `~=3.12`.
pub fn parse_pep440(spec: &str) -> Result<VersionSet, ConstraintError> {
    let mut set = VersionSet::any();
    for clause in spec.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        set = set.intersect(&pep440_clause(clause, spec)?);
    }
    Ok(set)
}

fn pep440_clause(clause: &str, spec: &str) -> Result<VersionSet, ConstraintError> {
    let err = |why: &str| ConstraintError(format!("`{spec}`: {why}"));
    let (op, rest) = ["===", "~=", "==", "!=", "<=", ">=", "<", ">"]
        .iter()
        .find_map(|op| clause.strip_prefix(op).map(|r| (*op, r.trim())))
        .ok_or_else(|| err("each clause needs an operator such as >= or =="))?;
    if rest.contains('!') {
        return Err(err("version epochs are not supported"));
    }
    let release = release_prefix(rest);
    if release.is_empty() {
        return Err(err("operator without a version"));
    }
    let p = parse_partial(release)?;
    let v = p.floor();
    Ok(match op {
        "==" | "===" if p.wildcard => p.x_range()?,
        "==" | "===" => VersionSet::exact(v),
        "!=" if p.wildcard => {
            let hi = p.upper_exclusive()?.ok_or_else(|| err("`!=*` excludes everything"))?;
            VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v))
                .union(VersionSet::interval(Bound::Inclusive(hi), Bound::Unbounded))
        }
        "!=" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v))
            .union(VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded)),
        "<=" => VersionSet::interval(Bound::Unbounded, Bound::Inclusive(v)),
        ">=" => VersionSet::interval(Bound::Inclusive(v), Bound::Unbounded),
        "<" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v)),
        ">" => VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded),
        "~=" => compatible(release, v, &err)?,
        _ => unreachable!("listed operators only"),
    })
}

/// `~=X.Y` → `>=X.Y, ==X.*`; `~=X.Y.Z` → `>=X.Y.Z, ==X.Y.*`.
fn compatible(release: &str, v: Version, err: &dyn Fn(&str) -> ConstraintError) -> Result<VersionSet, ConstraintError> {
    let hi = match release.split('.').count() {
        0 | 1 => return Err(err("~= needs at least two release segments")),
        2 => Version::new(bump(v.major)?, 0, 0),
        _ => Version::new(v.major, bump(v.minor)?, 0),
    };
    Ok(VersionSet::interval(Bound::Inclusive(v), Bound::Exclusive(hi)))
}

/// RubyGems requirements, e.g. `~> 3.3`, `>= 3.2, < 4`.
pub fn parse_gem_requirement(req: &str) -> Result<VersionSet, ConstraintError> {
    let mut set = VersionSet::any();
    let mut any = false;
    for clause in req.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        any = true;
        set = set.intersect(&gem_clause(clause, req)?);
    }
    if !any {
        return Err(ConstraintError(format!("`{req}`: empty requirement")));
    }
    Ok(set)
}

fn gem_clause(clause: &str, req: &str) -> Result<VersionSet, ConstraintError> {
    let (op, rest) = ["~>", ">=", "<=", "!=", "=", ">", "<"]
        .iter()
        .find_map(|op| clause.strip_prefix(op).map(|r| (*op, r.trim())))
        .unwrap_or(("=", clause));
    let release = release_prefix(rest);
    if release.is_empty() {
        return Err(ConstraintError(format!("`{req}`: operator without a version")));
    }
    let p = parse_partial(release)?;
    let v = p.floor();
    Ok(match op {
        "=" => VersionSet::exact(v),
        "!=" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v))
            .union(VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded)),
        ">=" => VersionSet::interval(Bound::Inclusive(v), Bound::Unbounded),
        "<=" => VersionSet::interval(Bound::Unbounded, Bound::Inclusive(v)),
        ">" => VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded),
        "<" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v)),
        "~>" => {
            // Pessimistic: drop the last segment and bump the one before it (`~> 3` → `< 4`).
            let hi = match (p.minor, p.patch) {
                (None, _) | (Some(_), None) => Version::new(bump(v.major)?, 0, 0),
                (Some(_), Some(_)) => Version::new(v.major, bump(v.minor)?, 0),
            };
            VersionSet::interval(Bound::Inclusive(v), Bound::Exclusive(hi))
        }
        _ => unreachable!("listed operators only"),
    })
}

/// Poetry's `python = "^3.11"` style constraints: npm caret/tilde semantics, comma as AND.
pub fn parse_poetry_constraint(c: &str) -> Result<VersionSet, ConstraintError> {
    let mut set = VersionSet::any();
    for clause in c.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        let alternatives: Result<Vec<VersionSet>, _> = clause.split("||").map(|a| npm_clause(a.trim())).collect();
        let union = alternatives?.into_iter().fold(VersionSet::none(), VersionSet::union);
        set = set.intersect(&union);
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(set: &VersionSet, v: (u64, u64, u64)) -> bool {
        set.contains(Version::new(v.0, v.1, v.2))
    }

    #[test]
    fn release_prefixes() {
        assert_eq!(release_prefix("3.12.0rc1"), "3.12.0");
        assert_eq!(release_prefix("3.3.6p108"), "3.3.6");
        assert_eq!(release_prefix("1.26"), "1.26");
        assert_eq!(release_prefix("v1.2."), "1.2");
        assert_eq!(release_prefix("stable"), "");
    }

    #[test]
    fn minimums() {
        let s = at_least("1.26").unwrap();
        assert!(has(&s, (1, 26, 0)) && has(&s, (1, 27, 3)) && !has(&s, (1, 25, 9)));
        assert!(at_least("nightly").is_err());
    }

    #[test]
    fn pep440() {
        let s = parse_pep440(">=3.11,<3.14").unwrap();
        assert!(has(&s, (3, 13, 9)) && !has(&s, (3, 14, 0)) && !has(&s, (3, 10, 9)));
        let s = parse_pep440("~=3.12").unwrap();
        assert!(has(&s, (3, 14, 0)) && !has(&s, (4, 0, 0)) && !has(&s, (3, 11, 9)));
        let s = parse_pep440("~=3.12.1").unwrap();
        assert!(has(&s, (3, 12, 5)) && !has(&s, (3, 13, 0)));
        let s = parse_pep440("==3.12.*").unwrap();
        assert!(has(&s, (3, 12, 9)) && !has(&s, (3, 13, 0)));
        let s = parse_pep440("!=3.13.*, >=3.11").unwrap();
        assert!(has(&s, (3, 12, 0)) && !has(&s, (3, 13, 2)) && has(&s, (3, 14, 0)));
        let s = parse_pep440(">=3.10.0rc1").unwrap();
        assert!(has(&s, (3, 10, 0)));
        assert!(parse_pep440("").unwrap().contains(Version::new(9, 9, 9)));
        for bad in ["3.12", "~=3", ">=", ">=1!2.0", ">=abc"] {
            assert!(parse_pep440(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rubygems() {
        let s = parse_gem_requirement("~> 3.3").unwrap();
        assert!(has(&s, (3, 4, 1)) && !has(&s, (4, 0, 0)) && !has(&s, (3, 2, 9)));
        let s = parse_gem_requirement("~> 3.3.6").unwrap();
        assert!(has(&s, (3, 3, 9)) && !has(&s, (3, 4, 0)));
        let s = parse_gem_requirement("~> 3").unwrap();
        assert!(has(&s, (3, 9, 0)) && !has(&s, (4, 0, 0)));
        let s = parse_gem_requirement(">= 3.2, < 4").unwrap();
        assert!(has(&s, (3, 4, 0)) && !has(&s, (4, 0, 0)));
        let s = parse_gem_requirement("3.3.6").unwrap();
        assert!(has(&s, (3, 3, 6)) && !has(&s, (3, 3, 7)));
        assert!(parse_gem_requirement("").is_err());
        assert!(parse_gem_requirement("~>").is_err());
    }

    #[test]
    fn poetry() {
        let s = parse_poetry_constraint("^3.11").unwrap();
        assert!(has(&s, (3, 13, 0)) && !has(&s, (4, 0, 0)));
        let s = parse_poetry_constraint(">=3.11,<3.13").unwrap();
        assert!(has(&s, (3, 12, 9)) && !has(&s, (3, 13, 0)));
        let s = parse_poetry_constraint("~3.12").unwrap();
        assert!(has(&s, (3, 12, 4)) && !has(&s, (3, 13, 0)));
    }
}
