//! Composer constraints (https://getcomposer.org/doc/articles/versions.md). Partial versions in
//! comparisons and exact constraints are padded with zeros, unlike npm.

use super::npm::caret;
use super::{Bound, ConstraintError, Version, VersionSet, bump, parse_partial};

pub fn parse_composer_constraint(input: &str) -> Result<VersionSet, ConstraintError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ConstraintError("empty constraint".into()));
    }
    let mut set = VersionSet::none();
    for alt in trimmed.split("||").flat_map(|p| p.split('|')) {
        let alt = alt.trim();
        if alt.is_empty() {
            return Err(ConstraintError(format!("`{input}`: empty alternative")));
        }
        set = set.union(parse_and(alt, input)?);
    }
    Ok(set)
}

fn parse_and(s: &str, input: &str) -> Result<VersionSet, ConstraintError> {
    let words: Vec<&str> = s.split([' ', ',']).filter(|w| !w.is_empty()).collect();
    if words.contains(&"-") {
        if words.len() != 3 || words[1] != "-" {
            return Err(ConstraintError(format!("`{input}`: a hyphen range is `A - B`")));
        }
        return hyphen(words[0], words[2]);
    }
    let mut set = VersionSet::any();
    let mut pending: Option<&str> = None;
    for w in words {
        if matches!(w, ">" | ">=" | "<" | "<=" | "=" | "==" | "!=" | "<>" | "~" | "^") {
            pending = Some(w);
            continue;
        }
        let token = match pending.take() {
            Some(op) => format!("{op}{w}"),
            None => w.to_string(),
        };
        set = set.intersect(&token_set(&token, input)?);
    }
    if pending.is_some() {
        return Err(ConstraintError(format!("`{input}`: operator without a version")));
    }
    Ok(set)
}

fn strip_stability(t: &str) -> &str {
    t.split('@').next().unwrap_or(t)
}

fn split_op(c: &str) -> (&str, &str) {
    for op in [">=", "<=", "<>", "!=", "==", ">", "<", "=", "^", "~"] {
        if let Some(rest) = c.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("", c)
}

fn token_set(token: &str, input: &str) -> Result<VersionSet, ConstraintError> {
    let token = strip_stability(token);
    if token == "*" {
        return Ok(VersionSet::any());
    }
    let (op, rest) = split_op(token);
    if rest.is_empty() {
        return Err(ConstraintError(format!("`{input}`: operator without a version")));
    }
    if rest.starts_with("dev-") {
        return Err(ConstraintError(format!(
            "`{input}`: branch constraints can't select a runtime version"
        )));
    }
    let p = parse_partial(rest)?;
    let v = p.floor();
    Ok(match op {
        "" | "=" | "==" => {
            if p.wildcard {
                p.x_range()?
            } else {
                VersionSet::exact(v)
            }
        }
        "!=" | "<>" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v))
            .union(VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded)),
        ">" => VersionSet::interval(Bound::Exclusive(v), Bound::Unbounded),
        ">=" => VersionSet::interval(Bound::Inclusive(v), Bound::Unbounded),
        "<" => VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v)),
        "<=" => VersionSet::interval(Bound::Unbounded, Bound::Inclusive(v)),
        "^" => caret(p)?,
        "~" => {
            let Some(major) = p.major else {
                return Ok(VersionSet::any());
            };
            let hi = match (p.minor, p.patch) {
                (Some(n), Some(_)) => Version::new(major, bump(n)?, 0),
                _ => Version::new(bump(major)?, 0, 0),
            };
            VersionSet::interval(Bound::Inclusive(v), Bound::Exclusive(hi))
        }
        _ => unreachable!("split_op only returns listed operators"),
    })
}

fn hyphen(a: &str, b: &str) -> Result<VersionSet, ConstraintError> {
    let lo = parse_partial(strip_stability(a))?;
    let hi = parse_partial(strip_stability(b))?;
    let upper = if hi.patch.is_some() {
        Bound::Inclusive(hi.floor())
    } else {
        match hi.upper_exclusive()? {
            Some(v) => Bound::Exclusive(v),
            None => Bound::Unbounded,
        }
    };
    Ok(VersionSet::interval(Bound::Inclusive(lo.floor()), upper))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Version;

    fn check(c: &str, yes: &[(u64, u64, u64)], no: &[(u64, u64, u64)]) {
        let set = parse_composer_constraint(c).unwrap_or_else(|e| panic!("{c}: {e}"));
        for &(a, b, p) in yes {
            assert!(set.contains(Version::new(a, b, p)), "{c} should contain {a}.{b}.{p}");
        }
        for &(a, b, p) in no {
            assert!(
                !set.contains(Version::new(a, b, p)),
                "{c} should not contain {a}.{b}.{p}"
            );
        }
    }

    #[test]
    fn composer_docs_examples() {
        check("1.0.2", &[(1, 0, 2)], &[(1, 0, 3)]);
        check(">=1.0", &[(1, 0, 0), (9, 0, 0)], &[(0, 9, 9)]);
        check(">=1.0 <2.0", &[(1, 5, 0)], &[(2, 0, 0)]);
        check(">=1.0,<2.0", &[(1, 5, 0)], &[(2, 0, 0)]);
        check(">=1.0 <1.1 || >=1.2", &[(1, 0, 5), (1, 2, 0)], &[(1, 1, 0)]);
        check("1.0 - 2.0", &[(1, 0, 0), (2, 0, 9)], &[(2, 1, 0)]);
        check("1.0.0 - 2.1.0", &[(2, 1, 0)], &[(2, 1, 1)]);
        check("1.0.*", &[(1, 0, 9)], &[(1, 1, 0)]);
        check("~1.2", &[(1, 2, 0), (1, 9, 0)], &[(2, 0, 0)]);
        check("~1.2.3", &[(1, 2, 3), (1, 2, 9)], &[(1, 3, 0)]);
        check("~1", &[(1, 5, 0)], &[(2, 0, 0)]);
        check("^1.2.3", &[(1, 9, 0)], &[(2, 0, 0), (1, 2, 2)]);
        check("^0.3", &[(0, 3, 9)], &[(0, 4, 0)]);
        check("*", &[(0, 0, 1), (8, 4, 0)], &[]);
    }

    #[test]
    fn php_requirements_seen_in_the_wild() {
        check("^8.2", &[(8, 2, 0), (8, 4, 3)], &[(9, 0, 0), (8, 1, 9)]);
        check(">=8.3 <8.5", &[(8, 3, 0), (8, 4, 9)], &[(8, 5, 0), (8, 2, 9)]);
        check("^8.2 || ^8.3", &[(8, 3, 1)], &[(7, 4, 0)]);
        check("^8.2|^8.3", &[(8, 3, 1)], &[(7, 4, 0)]);
        check("8.2.*", &[(8, 2, 20)], &[(8, 3, 0)]);
        check(">8.2", &[(8, 2, 1)], &[(8, 2, 0)]);
        check("<=8.2", &[(8, 2, 0)], &[(8, 2, 1)]);
        check("!=8.3.0", &[(8, 3, 1), (8, 2, 0)], &[(8, 3, 0)]);
        check("8.2", &[(8, 2, 0)], &[(8, 2, 1)]);
        check(">=8.2@dev", &[(8, 4, 0)], &[(8, 1, 0)]);
        check(">= 8.1", &[(8, 1, 0)], &[(8, 0, 9)]);
    }

    #[test]
    fn errors() {
        for bad in ["", "dev-master", "||", ">=8.2 ||", "^", "abc"] {
            assert!(parse_composer_constraint(bad).is_err(), "{bad:?}");
        }
    }
}
