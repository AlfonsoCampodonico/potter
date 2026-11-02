//! `apt` steps on catalog chains (spec §4.3): packages from Debian trixie as the snapshot the step
//! names, at snapshot.debian.org, so the same step installs the same bytes on every build.

use potter_plan::Step;

/// The archive the snapshot is taken from.
const SNAPSHOT: &str = "http://snapshot.debian.org/archive";

/// What an apt run leaves behind that differs on every run: dpkg's and apt's logs (timestamps)
/// and ldconfig's cache (inode numbers and mtimes). Removed, so the same step on the same
/// snapshot makes the same layer bytes.
const UNREPRODUCIBLE: &str = "/var/log/apt /var/log/dpkg.log /var/log/alternatives.log /var/cache/ldconfig/aux-cache";

/// `step` as a `Run` step when it is an `Apt` one; any other step unchanged. The plan validated
/// the package names and the snapshot, so both go into the script as they are.
pub fn lower(step: &Step) -> Step {
    let Step::Apt {
        name,
        packages,
        snapshot,
    } = step
    else {
        return step.clone();
    };
    let sources = format!(
        "Types: deb\n\
         URIs: {SNAPSHOT}/debian/{snapshot}/\n\
         Suites: trixie trixie-updates\n\
         Components: main\n\
         Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg\n\
         \n\
         Types: deb\n\
         URIs: {SNAPSHOT}/debian-security/{snapshot}/\n\
         Suites: trixie-security\n\
         Components: main\n\
         Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg\n"
    );
    let script = format!(
        "mkdir -p /etc/apt/sources.list.d && rm -f /etc/apt/sources.list /etc/apt/sources.list.d/* && \
         cat > /etc/apt/sources.list.d/debian.sources <<'POTTER' &&\n{sources}POTTER\n\
         apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Retries=3 update && \
         DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=3 install -y --no-install-recommends {} && \
         rm -rf /var/lib/apt/lists/* {UNREPRODUCIBLE}",
        packages.join(" ")
    );
    crate::stopgap::install(name, script)
}

#[cfg(test)]
mod tests {
    use potter_plan::{Command, DEFAULT_APT_SNAPSHOT};

    use super::*;

    #[test]
    fn other_steps_are_kept() {
        let meta = Step::Meta {
            name: "m".into(),
            env: Default::default(),
            workdir: Some("/app".into()),
            user: None,
            labels: Default::default(),
        };
        assert_eq!(lower(&meta), meta);
    }

    #[test]
    fn an_apt_step_is_a_root_run_with_network() {
        let apt = Step::Apt {
            name: "pkgs".into(),
            packages: vec!["curl".into(), "libpq5".into()],
            snapshot: DEFAULT_APT_SNAPSHOT.into(),
        };
        let Step::Run {
            name,
            command: Command::Shell(script),
            env,
            workdir,
            user,
            network,
            ..
        } = lower(&apt)
        else {
            panic!("a shell run step")
        };
        assert_eq!(name, "pkgs");
        assert!(network);
        assert_eq!((workdir.as_str(), user.as_str()), ("/", "0:0"));
        assert!(env.is_empty(), "the snapshot is in the script: {env:?}");
        assert!(script.contains("--no-install-recommends curl libpq5"), "{script}");
        assert!(
            script.ends_with(
                "rm -rf /var/lib/apt/lists/* /var/log/apt /var/log/dpkg.log /var/log/alternatives.log \
                 /var/cache/ldconfig/aux-cache"
            ),
            "{script}"
        );
        // Only the snapshot's sources are left, and a flaky mirror is retried.
        assert!(
            script.starts_with(
                "mkdir -p /etc/apt/sources.list.d && rm -f /etc/apt/sources.list /etc/apt/sources.list.d/* && "
            ),
            "{script}"
        );
        assert_eq!(script.matches("-o Acquire::Retries=3").count(), 2, "{script}");
    }
}
