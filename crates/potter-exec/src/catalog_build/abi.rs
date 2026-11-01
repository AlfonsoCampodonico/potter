//! The `os` and `libc` ABI values every entry carries, read from `os-min`'s files on the host:
//! nothing in the layer is executed.

use crate::error::BuildError;

fn plan(msg: String) -> BuildError {
    BuildError::Plan(msg)
}

/// `debian-<VERSION_ID>` from an `os-release` file.
pub fn os(os_release: &str) -> Result<String, BuildError> {
    let id = os_release
        .lines()
        .find_map(|l| l.trim().strip_prefix("ID="))
        .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\''));
    if id != Some("debian") {
        return Err(plan(format!(
            "os-min's os-release has ID {}, not debian",
            id.map_or("missing".to_string(), |i| format!("{i:?}"))
        )));
    }
    let version = os_release
        .lines()
        .find_map(|l| l.trim().strip_prefix("VERSION_ID="))
        .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\''))
        .filter(|v| !v.is_empty())
        .ok_or_else(|| plan("os-min's os-release has no VERSION_ID".into()))?;
    if !version.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.') {
        return Err(plan(format!(
            "os-min's os-release VERSION_ID {version:?} is not a version"
        )));
    }
    Ok(format!("debian-{version}"))
}

/// `glibc-<x.y>` from the `libc6` entry of a dpkg `status` file.
pub fn libc(status: &str) -> Result<String, BuildError> {
    let missing = || plan("os-min's /var/lib/dpkg/status has no installed libc6 entry with a version".into());
    let paragraph = status
        .split("\n\n")
        .find(|p| {
            p.lines().any(|l| l.trim_end() == "Package: libc6")
                && p.lines().any(|l| l.trim_end() == "Status: install ok installed")
        })
        .ok_or_else(missing)?;
    let version = paragraph
        .lines()
        .find_map(|l| l.strip_prefix("Version:"))
        .map(str::trim)
        .ok_or_else(missing)?;
    // [epoch:]upstream[-revision]
    let upstream = version.split_once(':').map_or(version, |(_, v)| v);
    let upstream = upstream.rsplit_once('-').map_or(upstream, |(u, _)| u);
    let mut parts = upstream.split('.');
    match (parts.next(), parts.next()) {
        (Some(x), Some(y))
            if !x.is_empty()
                && x.bytes().all(|b| b.is_ascii_digit())
                && !y.is_empty()
                && y.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Ok(format!("glibc-{x}.{y}"))
        }
        _ => Err(plan(format!("os-min's libc6 version {version:?} is not <x.y>…"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_is_debian_and_version_id() {
        let text =
            "PRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nNAME=\"Debian GNU/Linux\"\nVERSION_ID=\"13\"\nID=debian\n";
        assert_eq!(os(text).unwrap(), "debian-13");
        assert_eq!(os("ID=\"debian\"\nVERSION_ID=13\n").unwrap(), "debian-13");
        assert!(matches!(os("ID=debian\n"), Err(BuildError::Plan(m)) if m.contains("VERSION_ID")));
        assert!(matches!(
            os("ID=debian\nVERSION_ID=\"13 x\"\n"),
            Err(BuildError::Plan(_))
        ));
        assert!(matches!(os("ID=ubuntu\nVERSION_ID=24.04\n"), Err(BuildError::Plan(m)) if m.contains("not debian")));
        assert!(matches!(os("VERSION_ID=13\n"), Err(BuildError::Plan(m)) if m.contains("ID missing")));
    }

    #[test]
    fn libc_is_glibc_and_the_upstream_major_minor() {
        let status = "Package: base-files\nStatus: install ok installed\nVersion: 13.8\n\n\
                      Package: libc6\nStatus: install ok installed\nArchitecture: arm64\nVersion: 2.41-12\n\n\
                      Package: libc6-dev\nVersion: 9.99-1\n";
        assert_eq!(libc(status).unwrap(), "glibc-2.41");
        assert_eq!(
            libc("Package: libc6\nStatus: install ok installed\nVersion: 2.36-9+deb12u7\n").unwrap(),
            "glibc-2.36"
        );
        assert_eq!(
            libc("Package: libc6\nStatus: install ok installed\nVersion: 1:2.39.0-1\n").unwrap(),
            "glibc-2.39"
        );
        let err = libc("Package: libc6-dev\nVersion: 2.41-12\n").unwrap_err();
        assert!(
            matches!(&err, BuildError::Plan(m) if m.contains("no installed libc6 entry")),
            "{err:?}"
        );
        // Removed but its config kept: not installed.
        assert!(matches!(
            libc("Package: libc6\nStatus: deinstall ok config-files\nVersion: 2.41-12\n"),
            Err(BuildError::Plan(m)) if m.contains("no installed libc6")
        ));
        assert!(matches!(
            libc("Package: libc6\nStatus: install ok installed\n"),
            Err(BuildError::Plan(_))
        ));
        assert!(matches!(
            libc("Package: libc6\nStatus: install ok installed\nVersion: two\n"),
            Err(BuildError::Plan(_))
        ));
    }
}
