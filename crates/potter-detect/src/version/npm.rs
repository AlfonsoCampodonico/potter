//! node-semver ranges (https://github.com/npm/node-semver#ranges).

use super::{Bound, ConstraintError, Partial, Version, VersionSet, bump, parse_partial};

pub fn parse_npm_range(input: &str) -> Result<VersionSet, ConstraintError> {
    let mut set = VersionSet::none();
    for part in input.split("||") {
        set = set.union(parse_and(part.trim(), input)?);
    }
    Ok(set)
}

fn err(input: &str, why: &str) -> ConstraintError {
    ConstraintError(format!("`{input}`: {why}"))
}

fn parse_and(s: &str, input: &str) -> Result<VersionSet, ConstraintError> {
    if s.is_empty() {
        return Ok(VersionSet::any());
    }
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.contains(&"-") {
        if words.len() != 3 || words[1] != "-" {
            return Err(err(input, "a hyphen range is `A - B`"));
        }
        return hyphen(parse_partial(words[0])?, parse_partial(words[2])?);
    }
    let mut set = VersionSet::any();
    let mut pending: Option<&str> = None;
    for w in words {
        if is_operator(w) {
            if pending.is_some() {
                return Err(err(input, "two operators in a row"));
            }
            pending = Some(w);
            continue;
        }
        let comparator = match pending.take() {
            Some(op) => format!("{op}{w}"),
            None => w.to_string(),
        };
        set = set.intersect(&comparator_set(&comparator, input)?);
    }
    if pending.is_some() {
        return Err(err(input, "operator without a version"));
    }
    Ok(set)
}

fn is_operator(w: &str) -> bool {
    matches!(w, ">" | ">=" | "<" | "<=" | "=" | "~" | "~>" | "^")
}

fn split_op(c: &str) -> (&str, &str) {
    for op in [">=", "<=", "~>", ">", "<", "=", "~", "^"] {
        if let Some(rest) = c.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("", c)
}

fn comparator_set(c: &str, input: &str) -> Result<VersionSet, ConstraintError> {
    let (op, rest) = split_op(c);
    if rest.is_empty() {
        return Err(err(input, "operator without a version"));
    }
    let p = parse_partial(rest)?;
    let floor = p.floor();
    Ok(match op {
        "" | "=" => p.x_range()?,
        "^" => caret(p)?,
        "~" | "~>" => tilde(p)?,
        ">" => match p.upper_exclusive()? {
            None => VersionSet::none(),
            Some(hi) if p.patch.is_none() => VersionSet::interval(Bound::Inclusive(hi), Bound::Unbounded),
            Some(_) => VersionSet::interval(Bound::Exclusive(floor), Bound::Unbounded),
        },
        ">=" => match p.major {
            None => VersionSet::any(),
            Some(_) => VersionSet::interval(Bound::Inclusive(floor), Bound::Unbounded),
        },
        "<" => match p.major {
            None => VersionSet::none(),
            Some(_) => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(floor)),
        },
        "<=" => match p.upper_exclusive()? {
            None => VersionSet::any(),
            Some(hi) if p.patch.is_none() => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(hi)),
            Some(_) => VersionSet::interval(Bound::Unbounded, Bound::Inclusive(floor)),
        },
        _ => unreachable!("split_op only returns listed operators"),
    })
}

/// `^`: allow changes that don't modify the left-most non-zero component.
pub(crate) fn caret(p: Partial) -> Result<VersionSet, ConstraintError> {
    let Some(major) = p.major else {
        return Ok(VersionSet::any());
    };
    let lo = Bound::Inclusive(p.floor());
    let hi = match (major, p.minor, p.patch) {
        (m, _, _) if m > 0 => Version::new(bump(m)?, 0, 0),
        (_, None, _) => Version::new(1, 0, 0),
        (_, Some(n), _) if n > 0 => Version::new(0, bump(n)?, 0),
        (_, Some(_), None) => Version::new(0, 1, 0),
        (_, Some(_), Some(pt)) => Version::new(0, 0, bump(pt)?),
    };
    Ok(VersionSet::interval(lo, Bound::Exclusive(hi)))
}

/// npm `~`: patch-level changes if a minor is given, minor-level changes otherwise.
fn tilde(p: Partial) -> Result<VersionSet, ConstraintError> {
    let Some(major) = p.major else {
        return Ok(VersionSet::any());
    };
    let hi = match p.minor {
        None => Version::new(bump(major)?, 0, 0),
        Some(n) => Version::new(major, bump(n)?, 0),
    };
    Ok(VersionSet::interval(Bound::Inclusive(p.floor()), Bound::Exclusive(hi)))
}

fn hyphen(lo: Partial, hi: Partial) -> Result<VersionSet, ConstraintError> {
    let lower = match lo.major {
        None => Bound::Unbounded,
        Some(_) => Bound::Inclusive(lo.floor()),
    };
    let upper = match (hi.major, hi.patch) {
        (None, _) => Bound::Unbounded,
        (Some(_), Some(_)) => Bound::Inclusive(hi.floor()),
        (Some(_), None) => match hi.upper_exclusive()? {
            Some(v) => Bound::Exclusive(v),
            None => Bound::Unbounded,
        },
    };
    Ok(VersionSet::interval(lower, upper))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Version;

    fn check(range: &str, yes: &[(u64, u64, u64)], no: &[(u64, u64, u64)]) {
        let set = parse_npm_range(range).unwrap_or_else(|e| panic!("{range}: {e}"));
        for &(a, b, c) in yes {
            assert!(
                set.contains(Version::new(a, b, c)),
                "{range} should contain {a}.{b}.{c}"
            );
        }
        for &(a, b, c) in no {
            assert!(
                !set.contains(Version::new(a, b, c)),
                "{range} should not contain {a}.{b}.{c}"
            );
        }
    }

    #[test]
    fn node_semver_readme_examples() {
        check("1.2.3 - 2.3.4", &[(1, 2, 3), (2, 3, 4)], &[(1, 2, 2), (2, 3, 5)]);
        check("1.2 - 2.3.4", &[(1, 2, 0)], &[(1, 1, 9)]);
        check("1.2.3 - 2.3", &[(2, 3, 9)], &[(2, 4, 0)]);
        check("1.2.3 - 2", &[(2, 9, 9)], &[(3, 0, 0)]);
        check("*", &[(0, 0, 0), (99, 0, 0)], &[]);
        check("", &[(1, 0, 0)], &[]);
        check("1.x", &[(1, 0, 0), (1, 9, 9)], &[(2, 0, 0)]);
        check("1.2.x", &[(1, 2, 9)], &[(1, 3, 0)]);
        check("~1.2.3", &[(1, 2, 3), (1, 2, 9)], &[(1, 3, 0), (1, 2, 2)]);
        check("~1.2", &[(1, 2, 0)], &[(1, 3, 0)]);
        check("~1", &[(1, 9, 0)], &[(2, 0, 0)]);
        check("~0.2.3", &[(0, 2, 3)], &[(0, 3, 0)]);
        check("^1.2.3", &[(1, 2, 3), (1, 9, 9)], &[(2, 0, 0), (1, 2, 2)]);
        check("^0.2.3", &[(0, 2, 3)], &[(0, 3, 0)]);
        check("^0.0.3", &[(0, 0, 3)], &[(0, 0, 4)]);
        check("^1.2.x", &[(1, 2, 0)], &[(2, 0, 0)]);
        check("^0.0.x", &[(0, 0, 9)], &[(0, 1, 0)]);
        check("^0.0", &[(0, 0, 9)], &[(0, 1, 0)]);
        check("^1.x", &[(1, 0, 0)], &[(2, 0, 0)]);
        check("^0.x", &[(0, 9, 0)], &[(1, 0, 0)]);
    }

    #[test]
    fn comparators_with_partials() {
        check(">1", &[(2, 0, 0)], &[(1, 9, 9)]);
        check(">1.2", &[(1, 3, 0)], &[(1, 2, 9)]);
        check(">=1.2", &[(1, 2, 0)], &[(1, 1, 9)]);
        check("<1.2", &[(1, 1, 9)], &[(1, 2, 0)]);
        check("<=1.2", &[(1, 2, 9)], &[(1, 3, 0)]);
        check("<=1", &[(1, 9, 9)], &[(2, 0, 0)]);
        check(">=20.11", &[(20, 11, 0), (24, 0, 0)], &[(20, 10, 9)]);
        check("=1.2.3", &[(1, 2, 3)], &[(1, 2, 4)]);
        check("v1.2.3", &[(1, 2, 3)], &[(1, 2, 4)]);
    }

    #[test]
    fn combinations() {
        check(">=1.2.7 <1.3.0", &[(1, 2, 7), (1, 2, 99)], &[(1, 3, 0), (1, 2, 6)]);
        check(
            "1.2.7 || >=1.2.9 <2.0.0",
            &[(1, 2, 7), (1, 4, 6)],
            &[(1, 2, 8), (2, 0, 0)],
        );
        check(">= 18", &[(18, 0, 0)], &[(17, 9, 9)]);
        check(
            "^18 || ^20 || >=22",
            &[(20, 1, 0), (24, 0, 0)],
            &[(19, 0, 0), (21, 0, 0)],
        );
        check(">=1.0.0-beta.1", &[(1, 0, 0)], &[(0, 9, 0)]);
    }

    #[test]
    fn errors() {
        for bad in [">=", "1.2.3 -", "^", "1.2.3 - 2.0 - 3", "latest", ">=abc"] {
            assert!(parse_npm_range(bad).is_err(), "{bad:?}");
        }
    }
}
