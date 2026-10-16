//! Plan validation (spec §5.3). Collects every issue rather than stopping at the first.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Issue, PlanError};
use crate::ir::*;

const MAX_ID: u64 = (1 << 31) - 1;

pub fn validate(plan: &Plan) -> Result<(), PlanError> {
    let mut v = Validator::default();
    if plan.schema != SCHEMA {
        v.issue("schema", format!("expected `{SCHEMA}`, found `{}`", plan.schema));
    }
    let mut names = BTreeSet::new();
    for (i, t) in plan.targets.iter().enumerate() {
        let at = format!("targets[{i}]");
        v.name(&format!("{at}.name"), &t.name);
        if !names.insert(t.name.as_str()) {
            v.issue(&format!("{at}.name"), format!("duplicate target name `{}`", t.name));
        }
        if !is_relative_path(&t.root) {
            v.issue(&format!("{at}.root"), "must be a relative path without `..`");
        }
        v.name(&format!("{at}.provider"), &t.provider);
        v.chain(&format!("{at}.build"), &t.build);
        v.image(&format!("{at}.image"), &t.image);
    }
    if v.issues.is_empty() {
        Ok(())
    } else {
        Err(PlanError::Invalid(v.issues))
    }
}

/// `[a-z0-9._-]{1,64}`.
pub fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// Relative, no `..` components, no NUL; "." is allowed.
pub fn is_relative_path(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('/') && !s.contains('\0') && !s.split('/').any(|c| c == "..")
}

fn is_absolute_path(s: &str) -> bool {
    s.starts_with('/') && s.len() <= 4096 && !s.contains('\0') && !s.split('/').any(|c| c == "..")
}

fn is_numeric_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 10
        && s.bytes().all(|b| b.is_ascii_digit())
        && s.parse::<u64>().is_ok_and(|n| n <= MAX_ID)
}

fn is_user_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() || b == b'_' => {}
        _ => return false,
    }
    s.len() <= 32 && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn is_user(s: &str) -> bool {
    match s.split_once(':') {
        Some((u, g)) => is_numeric_id(u) && is_numeric_id(g),
        None => is_numeric_id(s) || is_user_name(s),
    }
}

fn is_owner(s: &str) -> bool {
    s.split_once(':')
        .is_some_and(|(u, g)| is_numeric_id(u) && is_numeric_id(g))
}

fn is_env_key(s: &str) -> bool {
    let mut bytes = s.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_digest(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

fn is_package(s: &str) -> bool {
    s.len() >= 2
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+.-".contains(&b))
}

fn is_snapshot(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 16 && b[8] == b'T' && b[15] == b'Z' && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit)
}

fn is_signal(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

fn is_version_label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b))
}

fn is_empty_argv(argv: &[String]) -> bool {
    argv.first().is_none_or(|s| s.trim().is_empty())
}

#[derive(Default)]
struct Validator {
    issues: Vec<Issue>,
}

impl Validator {
    fn issue(&mut self, at: &str, reason: impl Into<String>) {
        self.issues.push(Issue {
            at: at.to_string(),
            reason: reason.into(),
        });
    }

    fn name(&mut self, at: &str, s: &str) {
        if !is_name(s) {
            self.issue(at, format!("`{s}` must match [a-z0-9._-]{{1,64}}"));
        }
    }

    fn abs(&mut self, at: &str, s: &str) {
        if !is_absolute_path(s) {
            self.issue(at, "must be an absolute path without `..` or NUL");
        }
    }

    fn glob(&mut self, at: &str, g: &str) {
        if g.is_empty() || g.starts_with('/') || g.contains('\0') || g.split('/').any(|c| c == "..") {
            self.issue(at, "globs are relative to the target root, without `..`");
        } else if let Err(e) = globset::Glob::new(g) {
            self.issue(at, format!("invalid glob: {e}"));
        }
    }

    fn user(&mut self, at: &str, s: &str) {
        if !is_user(s) {
            self.issue(at, "must be a user name, a numeric id up to 2^31-1, or uid:gid");
        }
    }

    fn owner(&mut self, at: &str, s: &str) {
        if !is_owner(s) {
            self.issue(at, "must be numeric uid:gid, each up to 2^31-1");
        }
    }

    fn env(&mut self, at: &str, env: &BTreeMap<String, String>) {
        for (k, val) in env {
            if !is_env_key(k) {
                self.issue(&format!("{at}.{k}"), "environment keys match [A-Za-z_][A-Za-z0-9_]*");
            }
            if val.contains('\0') {
                self.issue(&format!("{at}.{k}"), "environment values cannot contain NUL");
            }
        }
    }

    fn command(&mut self, at: &str, c: &Command) {
        let empty = match c {
            Command::Argv(a) => is_empty_argv(a),
            Command::Shell(s) => s.trim().is_empty(),
        };
        if empty {
            self.issue(at, "command is empty");
        }
    }

    fn base(&mut self, at: &str, b: &Base) {
        match b {
            Base::Catalog { layers } => {
                if layers.is_empty() {
                    self.issue(&format!("{at}.layers"), "a catalog base needs at least one layer");
                }
                for (i, l) in layers.iter().enumerate() {
                    self.name(&format!("{at}.layers[{i}].name"), &l.name);
                    if let Some(v) = &l.version
                        && !is_version_label(v)
                    {
                        self.issue(&format!("{at}.layers[{i}].version"), format!("invalid version `{v}`"));
                    }
                }
            }
            Base::Oci { reference, digest } => {
                if reference.is_empty() || reference.chars().any(char::is_whitespace) {
                    self.issue(&format!("{at}.reference"), "invalid image reference");
                }
                if let Some(d) = digest
                    && !is_digest(d)
                {
                    self.issue(&format!("{at}.digest"), "digest must be sha256:<64 lowercase hex>");
                }
            }
            Base::Stage { name } => self.name(&format!("{at}.name"), name),
        }
    }

    fn step(&mut self, at: &str, s: &Step) {
        self.name(&format!("{at}.name"), s.name());
        match s {
            Step::Inputs {
                include,
                exclude,
                dest,
                owner,
                ..
            } => {
                if include.is_empty() {
                    self.issue(&format!("{at}.include"), "an inputs step needs at least one glob");
                }
                for (i, g) in include.iter().enumerate() {
                    self.glob(&format!("{at}.include[{i}]"), g);
                }
                for (i, g) in exclude.iter().enumerate() {
                    self.glob(&format!("{at}.exclude[{i}]"), g);
                }
                self.abs(&format!("{at}.dest"), dest);
                self.owner(&format!("{at}.owner"), owner);
            }
            Step::Run {
                command,
                env,
                workdir,
                user,
                cache_mounts,
                secrets,
                timeout_secs,
                ..
            } => {
                self.command(&format!("{at}.command"), command);
                self.env(&format!("{at}.env"), env);
                self.abs(&format!("{at}.workdir"), workdir);
                self.user(&format!("{at}.user"), user);
                for (i, m) in cache_mounts.iter().enumerate() {
                    self.name(&format!("{at}.cache_mounts[{i}].id"), &m.id);
                    self.abs(&format!("{at}.cache_mounts[{i}].target"), &m.target);
                }
                for (i, sec) in secrets.iter().enumerate() {
                    self.name(&format!("{at}.secrets[{i}].id"), &sec.id);
                    self.abs(&format!("{at}.secrets[{i}].target"), &sec.target);
                }
                if *timeout_secs == Some(0) {
                    self.issue(&format!("{at}.timeout_secs"), "timeout must be positive");
                }
            }
            Step::CopyFrom {
                from,
                paths,
                dest,
                owner,
                mode,
                ..
            } => {
                let (CopySource::Stage(n) | CopySource::Target(n)) = from;
                self.name(&format!("{at}.from"), n);
                for (i, p) in paths.iter().enumerate() {
                    self.abs(&format!("{at}.paths[{i}]"), p);
                }
                self.abs(&format!("{at}.dest"), dest);
                self.owner(&format!("{at}.owner"), owner);
                if mode.is_some_and(|m| m > 0o7777) {
                    self.issue(&format!("{at}.mode"), "mode must be at most 0o7777");
                }
            }
            Step::Apt { packages, snapshot, .. } => {
                if packages.is_empty() {
                    self.issue(&format!("{at}.packages"), "an apt step needs at least one package");
                }
                for (i, p) in packages.iter().enumerate() {
                    if !is_package(p) {
                        self.issue(
                            &format!("{at}.packages[{i}]"),
                            format!("`{p}` is not a Debian package name"),
                        );
                    }
                }
                if !is_snapshot(snapshot) {
                    self.issue(&format!("{at}.snapshot"), "snapshot must look like 20261001T000000Z");
                }
            }
            Step::Meta { env, workdir, user, .. } => {
                self.env(&format!("{at}.env"), env);
                if let Some(w) = workdir {
                    self.abs(&format!("{at}.workdir"), w);
                }
                if let Some(u) = user {
                    self.user(&format!("{at}.user"), u);
                }
            }
        }
    }

    fn chain(&mut self, at: &str, c: &Chain) {
        self.base(&format!("{at}.base"), &c.base);
        let mut names = BTreeSet::new();
        for (i, s) in c.steps.iter().enumerate() {
            let sat = format!("{at}.steps[{i}]");
            self.step(&sat, s);
            if !names.insert(s.name()) {
                self.issue(&format!("{sat}.name"), format!("duplicate step name `{}`", s.name()));
            }
        }
    }

    fn image(&mut self, at: &str, img: &Image) {
        self.base(&format!("{at}.base"), &img.base);
        for (i, s) in img.runtime_steps.iter().enumerate() {
            let sat = format!("{at}.runtime_steps[{i}]");
            if matches!(s, Step::Apt { .. }) {
                self.step(&sat, s);
            } else {
                self.issue(&sat, "runtime steps may only be apt steps");
            }
        }
        for (i, e) in img.export.iter().enumerate() {
            self.abs(&format!("{at}.export[{i}].path"), &e.path);
            self.abs(&format!("{at}.export[{i}].dest"), &e.dest);
            for (j, g) in e.exclude.iter().enumerate() {
                self.glob(&format!("{at}.export[{i}].exclude[{j}]"), g);
            }
        }
        let mut paths = BTreeSet::new();
        for (i, f) in img.files.iter().enumerate() {
            self.abs(&format!("{at}.files[{i}].path"), &f.path);
            if f.mode > 0o7777 {
                self.issue(&format!("{at}.files[{i}].mode"), "mode must be at most 0o7777");
            }
            if !paths.insert(f.path.as_str()) {
                self.issue(&format!("{at}.files[{i}].path"), "duplicate generated file");
            }
        }
        if let Some(why) = img.max_layers.and_then(crate::budget::check_max_layers) {
            self.issue(&format!("{at}.max_layers"), why);
        }
        self.env(&format!("{at}.config.env"), &img.config.env);
        self.abs(&format!("{at}.config.workdir"), &img.config.workdir);
        self.user(&format!("{at}.config.user"), &img.config.user);
        if img.config.port == Some(0) {
            self.issue(&format!("{at}.config.port"), "port must be 1-65535");
        }
        if let Some(sig) = &img.config.stop_signal
            && !is_signal(sig)
        {
            self.issue(
                &format!("{at}.config.stop_signal"),
                "signals are upper-case names like SIGQUIT",
            );
        }
        for (name, p) in &img.processes {
            self.name(&format!("{at}.processes.{name}"), name);
            if is_empty_argv(&p.command) {
                self.issue(&format!("{at}.processes.{name}.command"), "command is empty");
            }
            self.env(&format!("{at}.processes.{name}.env"), &p.env);
            if let Some(w) = &p.workdir {
                self.abs(&format!("{at}.processes.{name}.workdir"), w);
            }
        }
        for (process, services) in &img.services {
            if !img.processes.contains_key(process) {
                self.issue(
                    &format!("{at}.services.{process}"),
                    format!("no process named `{process}`"),
                );
            }
            let mut names = BTreeSet::new();
            for (i, s) in services.iter().enumerate() {
                let sat = format!("{at}.services.{process}[{i}]");
                self.name(&format!("{sat}.name"), &s.name);
                if !names.insert(s.name.as_str()) {
                    self.issue(&format!("{sat}.name"), "duplicate service name");
                }
                if is_empty_argv(&s.command) {
                    self.issue(&format!("{sat}.command"), "command is empty");
                }
                if let Ready::Unix(p) = &s.ready {
                    self.abs(&format!("{sat}.ready"), p);
                }
                if !is_signal(&s.stop_signal) {
                    self.issue(
                        &format!("{sat}.stop_signal"),
                        "signals are upper-case names like SIGQUIT",
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sample_plan;

    fn issues(plan: &Plan) -> Vec<Issue> {
        match validate(plan) {
            Ok(()) => vec![],
            Err(PlanError::Invalid(issues)) => issues,
            Err(other) => panic!("unexpected error {other}"),
        }
    }

    fn has(issues: &[Issue], at: &str) -> bool {
        issues.iter().any(|i| i.at == at)
    }

    #[test]
    fn sample_is_valid() {
        assert_eq!(issues(&sample_plan()), vec![]);
    }

    #[test]
    fn wrong_schema() {
        let mut p = sample_plan();
        p.schema = "potter.plan/v0".into();
        assert!(has(&issues(&p), "schema"));
    }

    #[test]
    fn bad_and_duplicate_names() {
        let mut p = sample_plan();
        p.targets.push(p.targets[0].clone());
        assert!(has(&issues(&p), "targets[1].name"));
        let mut p = sample_plan();
        p.targets[0].name = "App".into();
        assert!(has(&issues(&p), "targets[0].name"));
        let mut p = sample_plan();
        let dup = p.targets[0].build.steps[1].clone();
        p.targets[0].build.steps.push(dup);
        assert!(has(&issues(&p), "targets[0].build.steps[2].name"));
    }

    #[test]
    fn paths() {
        let mut p = sample_plan();
        p.targets[0].root = "../escape".into();
        assert!(has(&issues(&p), "targets[0].root"));
        let mut p = sample_plan();
        p.targets[0].image.export[0].dest = "relative".into();
        assert!(has(&issues(&p), "targets[0].image.export[0].dest"));
        let mut p = sample_plan();
        p.targets[0].image.export[0].path = "/app/../etc".into();
        assert!(has(&issues(&p), "targets[0].image.export[0].path"));
        let mut p = sample_plan();
        p.targets[0].image.files[0].path = "/etc/\0x".into();
        assert!(has(&issues(&p), "targets[0].image.files[0].path"));
    }

    #[test]
    fn globs() {
        let mut p = sample_plan();
        if let Step::Inputs { include, .. } = &mut p.targets[0].build.steps[0] {
            include.push("/abs/**".into());
            include.push("a/[".into());
        }
        let found = issues(&p);
        assert!(has(&found, "targets[0].build.steps[0].include[2]"));
        assert!(has(&found, "targets[0].build.steps[0].include[3]"));
    }

    #[test]
    fn users_and_owners() {
        for (user, ok) in [
            ("root", true),
            ("www-data", true),
            ("1000", true),
            ("1000:1000", true),
            ("2147483648", false),
            ("Root", false),
            ("", false),
        ] {
            let mut p = sample_plan();
            if let Step::Run { user: u, .. } = &mut p.targets[0].build.steps[1] {
                *u = user.into();
            }
            assert_eq!(issues(&p).is_empty(), ok, "user {user:?}");
        }
        let mut p = sample_plan();
        if let Step::Inputs { owner, .. } = &mut p.targets[0].build.steps[0] {
            *owner = "root".into();
        }
        assert!(has(&issues(&p), "targets[0].build.steps[0].owner"));
    }

    #[test]
    fn env_keys() {
        let mut p = sample_plan();
        p.targets[0].image.config.env.insert("1BAD".into(), "x".into());
        assert!(has(&issues(&p), "targets[0].image.config.env.1BAD"));
    }

    #[test]
    fn runtime_steps_only_apt() {
        let mut p = sample_plan();
        let run = p.targets[0].build.steps[1].clone();
        p.targets[0].image.runtime_steps.push(run);
        assert!(has(&issues(&p), "targets[0].image.runtime_steps[0]"));
    }

    #[test]
    fn apt_fields() {
        let mut p = sample_plan();
        p.targets[0].image.runtime_steps.push(Step::Apt {
            name: "apt-runtime".into(),
            packages: vec!["libpq5".into(), "Bad Name".into()],
            snapshot: "2026-10-01".into(),
        });
        let found = issues(&p);
        assert!(has(&found, "targets[0].image.runtime_steps[0].packages[1]"));
        assert!(has(&found, "targets[0].image.runtime_steps[0].snapshot"));
    }

    #[test]
    fn services_need_a_process() {
        let mut p = sample_plan();
        let svc = p.targets[0].image.services.remove("web").unwrap();
        p.targets[0].image.services.insert("worker".into(), svc);
        assert!(has(&issues(&p), "targets[0].image.services.worker"));
    }

    #[test]
    fn empty_commands() {
        let mut p = sample_plan();
        p.targets[0].image.processes.insert("worker".into(), Process::new(&[]));
        assert!(has(&issues(&p), "targets[0].image.processes.worker.command"));
        let mut p = sample_plan();
        if let Step::Run { command, .. } = &mut p.targets[0].build.steps[1] {
            *command = Command::Shell("  ".into());
        }
        assert!(has(&issues(&p), "targets[0].build.steps[1].command"));
    }

    #[test]
    fn oci_digest() {
        let mut p = sample_plan();
        p.targets[0].image.base = Base::Oci {
            reference: "php:8.4-fpm".into(),
            digest: Some("sha256:abc".into()),
        };
        assert!(has(&issues(&p), "targets[0].image.base.digest"));
    }

    #[test]
    fn file_modes() {
        let mut p = sample_plan();
        p.targets[0].image.files[0].mode = 0o17777;
        assert!(has(&issues(&p), "targets[0].image.files[0].mode"));
    }

    #[test]
    fn max_layers_bounds() {
        for (n, ok) in [(2, false), (3, true), (10, true), (128, true), (129, false)] {
            let mut p = sample_plan();
            p.targets[0].image.max_layers = Some(n);
            assert_eq!(!has(&issues(&p), "targets[0].image.max_layers"), ok, "max_layers {n}");
        }
    }

    #[test]
    fn helper_predicates() {
        assert!(is_name("deps-inputs"));
        assert!(is_name("user-build-1"));
        assert!(!is_name(""));
        assert!(!is_name(&"a".repeat(65)));
        assert!(is_relative_path("."));
        assert!(is_relative_path("services/api"));
        assert!(!is_relative_path("/abs"));
        assert!(!is_relative_path("a/../b"));
    }
}
