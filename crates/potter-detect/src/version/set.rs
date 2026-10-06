use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Version { major, minor, patch }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    Unbounded,
    Inclusive(Version),
    Exclusive(Version),
}

impl Bound {
    fn parts(self) -> Option<(Version, bool)> {
        match self {
            Bound::Unbounded => None,
            Bound::Inclusive(v) => Some((v, false)),
            Bound::Exclusive(v) => Some((v, true)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub lo: Bound,
    pub hi: Bound,
}

impl Interval {
    pub const ALL: Interval = Interval {
        lo: Bound::Unbounded,
        hi: Bound::Unbounded,
    };

    pub fn is_empty(&self) -> bool {
        match (self.lo.parts(), self.hi.parts()) {
            (None, _) | (_, None) => false,
            (Some((a, false)), Some((b, false))) => a > b,
            (Some((a, _)), Some((b, _))) => a >= b,
        }
    }

    pub fn contains(&self, v: Version) -> bool {
        let lo_ok = match self.lo {
            Bound::Unbounded => true,
            Bound::Inclusive(a) => v >= a,
            Bound::Exclusive(a) => v > a,
        };
        let hi_ok = match self.hi {
            Bound::Unbounded => true,
            Bound::Inclusive(b) => v <= b,
            Bound::Exclusive(b) => v < b,
        };
        lo_ok && hi_ok
    }

    pub fn intersect(&self, other: &Interval) -> Interval {
        Interval {
            lo: tighter_lo(self.lo, other.lo),
            hi: tighter_hi(self.hi, other.hi),
        }
    }
}

/// The larger lower bound; at equal versions an exclusive bound is tighter.
fn tighter_lo(a: Bound, b: Bound) -> Bound {
    match (a.parts(), b.parts()) {
        (None, _) => b,
        (_, None) => a,
        (Some((va, ea)), Some((vb, _))) => match va.cmp(&vb) {
            Ordering::Greater => a,
            Ordering::Less => b,
            Ordering::Equal => {
                if ea {
                    a
                } else {
                    b
                }
            }
        },
    }
}

/// The smaller upper bound; at equal versions an exclusive bound is tighter.
fn tighter_hi(a: Bound, b: Bound) -> Bound {
    match (a.parts(), b.parts()) {
        (None, _) => b,
        (_, None) => a,
        (Some((va, ea)), Some((vb, _))) => match va.cmp(&vb) {
            Ordering::Less => a,
            Ordering::Greater => b,
            Ordering::Equal => {
                if ea {
                    a
                } else {
                    b
                }
            }
        },
    }
}

/// A union of intervals. Never stores empty intervals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSet {
    intervals: Vec<Interval>,
}

impl VersionSet {
    pub fn any() -> Self {
        VersionSet {
            intervals: vec![Interval::ALL],
        }
    }

    pub fn none() -> Self {
        VersionSet { intervals: vec![] }
    }

    pub fn interval(lo: Bound, hi: Bound) -> Self {
        let i = Interval { lo, hi };
        if i.is_empty() {
            Self::none()
        } else {
            VersionSet { intervals: vec![i] }
        }
    }

    pub fn exact(v: Version) -> Self {
        Self::interval(Bound::Inclusive(v), Bound::Inclusive(v))
    }

    pub fn intersect(&self, other: &Self) -> Self {
        let mut intervals = Vec::new();
        for a in &self.intervals {
            for b in &other.intervals {
                let i = a.intersect(b);
                if !i.is_empty() {
                    intervals.push(i);
                }
            }
        }
        VersionSet { intervals }
    }

    pub fn union(mut self, other: Self) -> Self {
        self.intervals.extend(other.intervals);
        self
    }

    pub fn contains(&self, v: Version) -> bool {
        self.intervals.iter().any(|i| i.contains(v))
    }

    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    pub fn intersects(&self, other: &Interval) -> bool {
        self.intervals.iter().any(|i| !i.intersect(other).is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn v(a: u64, b: u64, c: u64) -> Version {
        Version::new(a, b, c)
    }

    #[test]
    fn interval_bounds() {
        let i = Interval {
            lo: Bound::Inclusive(v(1, 2, 0)),
            hi: Bound::Exclusive(v(2, 0, 0)),
        };
        assert!(i.contains(v(1, 2, 0)));
        assert!(i.contains(v(1, 99, 99)));
        assert!(!i.contains(v(2, 0, 0)));
        assert!(!i.contains(v(1, 1, 9)));
    }

    #[test]
    fn empty_intervals() {
        assert!(
            Interval {
                lo: Bound::Inclusive(v(2, 0, 0)),
                hi: Bound::Exclusive(v(2, 0, 0))
            }
            .is_empty()
        );
        assert!(
            !Interval {
                lo: Bound::Inclusive(v(2, 0, 0)),
                hi: Bound::Inclusive(v(2, 0, 0))
            }
            .is_empty()
        );
        assert!(
            Interval {
                lo: Bound::Exclusive(v(2, 0, 0)),
                hi: Bound::Inclusive(v(2, 0, 0))
            }
            .is_empty()
        );
        assert!(!Interval::ALL.is_empty());
    }

    #[test]
    fn set_operations() {
        let a = VersionSet::interval(Bound::Inclusive(v(8, 2, 0)), Bound::Unbounded);
        let b = VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v(8, 5, 0)));
        let both = a.intersect(&b);
        assert!(both.contains(v(8, 4, 1)));
        assert!(!both.contains(v(8, 5, 0)));
        assert!(!both.contains(v(8, 1, 9)));
        let either = VersionSet::exact(v(1, 0, 0)).union(VersionSet::exact(v(3, 0, 0)));
        assert!(either.contains(v(3, 0, 0)));
        assert!(!either.contains(v(2, 0, 0)));
        assert!(VersionSet::none().is_empty());
        assert!(VersionSet::interval(Bound::Inclusive(v(2, 0, 0)), Bound::Exclusive(v(1, 0, 0))).is_empty());
    }

    #[test]
    fn intersects_a_line() {
        let line = Interval {
            lo: Bound::Inclusive(v(8, 2, 0)),
            hi: Bound::Exclusive(v(8, 3, 0)),
        };
        assert!(VersionSet::interval(Bound::Unbounded, Bound::Exclusive(v(8, 2, 3))).intersects(&line));
        assert!(!VersionSet::interval(Bound::Inclusive(v(8, 3, 0)), Bound::Unbounded).intersects(&line));
    }

    fn bound() -> impl Strategy<Value = Bound> {
        let ver = (0u64..4, 0u64..4, 0u64..4).prop_map(|(a, b, c)| Version::new(a, b, c));
        prop_oneof![
            Just(Bound::Unbounded),
            ver.clone().prop_map(Bound::Inclusive),
            ver.prop_map(Bound::Exclusive)
        ]
    }

    proptest! {
        #[test]
        fn intersect_is_conjunction(a in bound(), b in bound(), c in bound(), d in bound(), x in 0u64..4, y in 0u64..4, z in 0u64..4) {
            let s1 = VersionSet::interval(a, b);
            let s2 = VersionSet::interval(c, d);
            let ver = Version::new(x, y, z);
            prop_assert_eq!(s1.intersect(&s2).contains(ver), s1.contains(ver) && s2.contains(ver));
            prop_assert_eq!(s1.clone().union(s2.clone()).contains(ver), s1.contains(ver) || s2.contains(ver));
        }
    }
}
