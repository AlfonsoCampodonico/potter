//! Runtime version lines available until the real catalog (P3) replaces this table (spec §7).

use potter_plan::{Decision, Origin};

use crate::error::DetectError;
use crate::version::{Bound, Interval, Version, VersionSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub version: &'static str,
    pub major: u64,
    pub minor: Option<u64>,
    pub lts: bool,
}

impl Line {
    /// Every version this line can resolve to.
    pub fn interval(&self) -> Interval {
        match self.minor {
            Some(m) => Interval {
                lo: Bound::Inclusive(Version::new(self.major, m, 0)),
                hi: Bound::Exclusive(Version::new(self.major, m + 1, 0)),
            },
            None => Interval {
                lo: Bound::Inclusive(Version::new(self.major, 0, 0)),
                hi: Bound::Exclusive(Version::new(self.major + 1, 0, 0)),
            },
        }
    }
}

#[derive(Debug)]
pub struct Runtime {
    pub name: &'static str,
    pub lines: &'static [Line],
    pub default: &'static str,
}

const fn line(version: &'static str, major: u64, minor: Option<u64>, lts: bool) -> Line {
    Line {
        version,
        major,
        minor,
        lts,
    }
}

pub static PHP: Runtime = Runtime {
    name: "php",
    lines: &[
        line("8.2", 8, Some(2), false),
        line("8.3", 8, Some(3), false),
        line("8.4", 8, Some(4), false),
    ],
    default: "8.4",
};

pub static NODE: Runtime = Runtime {
    name: "node",
    lines: &[
        line("20", 20, None, true),
        line("22", 22, None, true),
        line("24", 24, None, true),
    ],
    default: "22",
};

pub static BUN: Runtime = Runtime {
    name: "bun",
    lines: &[line("1", 1, None, false)],
    default: "1",
};

pub static DENO: Runtime = Runtime {
    name: "deno",
    lines: &[line("2", 2, None, false)],
    default: "2",
};

pub const COMPOSER_VERSION: &str = "2";

/// Where a version requirement came from, for `--explain` and error messages.
#[derive(Debug, Clone)]
pub struct VersionSource {
    /// Human-readable origin, e.g. `composer.json require.php = ^8.2`.
    pub desc: String,
    pub origin: Origin,
    pub set: VersionSet,
    pub lts_only: bool,
}

/// The highest line satisfying the source, or the runtime's default when there is no source.
pub fn pick(
    rt: &'static Runtime,
    field: &str,
    source: Option<VersionSource>,
    decisions: &mut Vec<Decision>,
) -> Result<&'static Line, DetectError> {
    let Some(src) = source else {
        let line = rt
            .lines
            .iter()
            .find(|l| l.version == rt.default)
            .expect("default is one of the lines");
        decisions.push(Decision::default(field, line.version));
        return Ok(line);
    };
    let best = rt
        .lines
        .iter()
        .filter(|l| (!src.lts_only || l.lts) && src.set.intersects(&l.interval()))
        .max_by_key(|l| (l.major, l.minor));
    match best {
        Some(line) => {
            decisions.push(Decision {
                field: field.to_string(),
                value: line.version.to_string(),
                origin: src.origin,
            });
            Ok(line)
        }
        None => Err(DetectError::Unsatisfiable {
            runtime: rt.name,
            source_desc: src.desc,
            available: rt.lines.iter().map(|l| l.version).collect::<Vec<_>>().join(", "),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::{parse_composer_constraint, parse_npm_range};
    use potter_plan::Origin;

    fn src(set: crate::version::VersionSet) -> Option<VersionSource> {
        Some(VersionSource {
            desc: "test".into(),
            origin: Origin::Default,
            set,
            lts_only: false,
        })
    }

    #[test]
    fn highest_satisfying_line() {
        let mut d = vec![];
        assert_eq!(
            pick(
                &PHP,
                "runtime.php",
                src(parse_composer_constraint("^8.2").unwrap()),
                &mut d
            )
            .unwrap()
            .version,
            "8.4"
        );
        assert_eq!(
            pick(
                &PHP,
                "runtime.php",
                src(parse_composer_constraint(">=8.2 <8.4").unwrap()),
                &mut d
            )
            .unwrap()
            .version,
            "8.3"
        );
        assert_eq!(
            pick(&NODE, "runtime.node", src(parse_npm_range(">=20.11").unwrap()), &mut d)
                .unwrap()
                .version,
            "24"
        );
        assert_eq!(d.len(), 3);
    }

    #[test]
    fn default_without_constraint() {
        let mut d = vec![];
        assert_eq!(pick(&NODE, "runtime.node", None, &mut d).unwrap().version, "22");
        assert_eq!(d[0].origin, Origin::Default);
    }

    #[test]
    fn lts_only() {
        let mut d = vec![];
        let s = VersionSource {
            desc: ".nvmrc = lts/*".into(),
            origin: Origin::Default,
            set: crate::version::VersionSet::any(),
            lts_only: true,
        };
        assert_eq!(pick(&NODE, "runtime.node", Some(s), &mut d).unwrap().version, "24");
    }

    #[test]
    fn unsatisfiable_lists_available() {
        let mut d = vec![];
        let e = pick(
            &PHP,
            "runtime.php",
            src(parse_composer_constraint("^7.4").unwrap()),
            &mut d,
        )
        .unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("8.2, 8.3, 8.4"), "{msg}");
        assert!(msg.contains("test"), "{msg}");
    }

    #[test]
    fn provider_ids_round_trip() {
        for id in crate::ProviderId::ALL {
            assert_eq!(crate::ProviderId::parse(id.as_str()), Some(id));
        }
        assert_eq!(crate::ProviderId::parse("cobol"), None);
    }
}
