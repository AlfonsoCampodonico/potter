//! The overlap check (design §6.3): no regular file is in two layers that can be installed
//! together, except where `build-tools` replaces `os-min`'s dpkg state and docs.

use std::collections::BTreeSet;

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::recipe::OS_MIN;

/// The layer that may replace some of `os-min`'s files.
pub const BUILD_TOOLS: &str = "build-tools";
/// The paths `build-tools` may share with `os-min`.
pub const ALLOWED: [&str; 3] = ["var/lib/dpkg/**", "usr/share/doc/**", "etc/ld.so.cache"];
/// The most shared paths a failure names per pair of layers.
pub const SHOWN: usize = 20;

/// One built layer, for the check.
pub struct Layer<'a> {
    pub name: &'a str,
    pub conflicts: &'a [String],
    /// Its regular files, without a leading `/`.
    pub files: &'a BTreeSet<String>,
}

fn allowed() -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for p in ALLOWED {
        b.add(Glob::new(p).expect("a valid glob"));
    }
    b.build().expect("valid globs")
}

/// Whether `a` and `b` can be installed together: `os-min` is under every layer, so it pairs
/// with each one whatever the conflicts say.
fn co_installable(a: &Layer, b: &Layer) -> bool {
    a.name == OS_MIN
        || b.name == OS_MIN
        || !(a.conflicts.iter().any(|c| c == b.name) || b.conflicts.iter().any(|c| c == a.name))
}

/// One line per pair of co-installable layers that share a regular file, naming both layers and
/// the first [`SHOWN`] shared paths; empty when there is no overlap.
pub fn check(layers: &[Layer]) -> Vec<String> {
    let allowed = allowed();
    let mut out = Vec::new();
    for (i, a) in layers.iter().enumerate() {
        for b in &layers[i + 1..] {
            if !co_installable(a, b) {
                continue;
            }
            let exempt = [a.name, b.name].contains(&OS_MIN) && [a.name, b.name].contains(&BUILD_TOOLS);
            let shared: Vec<&String> = a
                .files
                .intersection(b.files)
                .filter(|p| !(exempt && allowed.is_match(p.as_str())))
                .collect();
            if shared.is_empty() {
                continue;
            }
            let mut shown: Vec<String> = shared.iter().take(SHOWN).map(|p| format!("/{p}")).collect();
            if shared.len() > SHOWN {
                shown.push(format!("and {} more", shared.len() - SHOWN));
            }
            out.push(format!(
                "{} and {} both hold {} regular file{}: {}",
                a.name,
                b.name,
                shared.len(),
                if shared.len() == 1 { "" } else { "s" },
                shown.join(", ")
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn disjoint_layers_pass() {
        let (os, py) = (files(&["etc/os-release", "bin/sh"]), files(&["usr/local/bin/python3"]));
        let none: Vec<String> = vec![];
        let layers = [
            Layer {
                name: "os-min",
                conflicts: &none,
                files: &os,
            },
            Layer {
                name: "python",
                conflicts: &none,
                files: &py,
            },
        ];
        assert!(check(&layers).is_empty());
    }

    #[test]
    fn a_shared_file_names_both_layers_and_the_path() {
        let (a, b) = (
            files(&["usr/local/bin/x", "usr/local/lib/y"]),
            files(&["usr/local/bin/x"]),
        );
        let none: Vec<String> = vec![];
        let layers = [
            Layer {
                name: "node",
                conflicts: &none,
                files: &a,
            },
            Layer {
                name: "bun",
                conflicts: &none,
                files: &b,
            },
        ];
        assert_eq!(
            check(&layers),
            ["node and bun both hold 1 regular file: /usr/local/bin/x"]
        );
    }

    #[test]
    fn only_the_first_twenty_paths_are_named() {
        let many: BTreeSet<String> = (0..25).map(|i| format!("usr/f{i:02}")).collect();
        let none: Vec<String> = vec![];
        let layers = [
            Layer {
                name: "a",
                conflicts: &none,
                files: &many,
            },
            Layer {
                name: "b",
                conflicts: &none,
                files: &many,
            },
        ];
        let [line] = check(&layers).try_into().unwrap();
        assert!(
            line.starts_with("a and b both hold 25 regular files: /usr/f00, "),
            "{line}"
        );
        assert!(line.contains("/usr/f19, and 5 more"), "{line}");
        assert!(!line.contains("/usr/f20"), "{line}");
    }

    #[test]
    fn conflicting_layers_may_overlap_but_never_with_os_min() {
        let shared = files(&["usr/local/bin/python3"]);
        let (c, none) = (vec!["python-alt".to_string()], vec![]);
        let layers = [
            Layer {
                name: "python",
                conflicts: &c,
                files: &shared,
            },
            Layer {
                name: "python-alt",
                conflicts: &none,
                files: &shared,
            },
        ];
        assert!(check(&layers).is_empty());
        let os_conflict = vec!["python".to_string()];
        let layers = [
            Layer {
                name: "os-min",
                conflicts: &os_conflict,
                files: &shared,
            },
            Layer {
                name: "python",
                conflicts: &c,
                files: &shared,
            },
        ];
        assert_eq!(check(&layers).len(), 1, "os-min is always installed");
    }

    #[test]
    fn build_tools_may_replace_os_min_dpkg_state_and_docs_only() {
        let os = files(&[
            "var/lib/dpkg/status",
            "usr/share/doc/libc6/copyright",
            "etc/ld.so.cache",
            "usr/bin/ld",
        ]);
        let tools = os.clone();
        let none: Vec<String> = vec![];
        let layers = [
            Layer {
                name: "os-min",
                conflicts: &none,
                files: &os,
            },
            Layer {
                name: "build-tools",
                conflicts: &none,
                files: &tools,
            },
        ];
        assert_eq!(
            check(&layers),
            ["os-min and build-tools both hold 1 regular file: /usr/bin/ld"]
        );
        // The exemption is build-tools' alone.
        let layers = [
            Layer {
                name: "os-min",
                conflicts: &none,
                files: &os,
            },
            Layer {
                name: "python",
                conflicts: &none,
                files: &tools,
            },
        ];
        assert!(check(&layers)[0].contains("4 regular files"));
    }
}
