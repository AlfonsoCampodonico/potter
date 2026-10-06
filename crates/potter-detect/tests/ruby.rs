mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions};

#[test]
fn rails_with_propshaft_and_bootsnap() {
    let d = detect("ruby-rails");
    let t = &d.target;
    assert_eq!(t.provider, "ruby");
    assert_eq!(decision(&d, "runtime.ruby").value, "3.3");
    assert_eq!(layers(&t.build.base), ["os-min", "ruby@3.3"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "bootsnap", "assets"]);
    assert_eq!(command(t, "deps"), "bundle install");
    assert_eq!(command(t, "assets"), "bin/rails assets:precompile");
    assert_eq!(process(t, "web"), "bin/rails server -b 0.0.0.0");
    assert_eq!(t.image.config.env["RAILS_ENV"], "production");
    assert_eq!(t.image.config.env["BUNDLE_PATH"], "/app/vendor/bundle");
    assert_eq!(t.image.config.port, Some(3000));
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn sinatra_with_pg() {
    let d = detect("ruby-sinatra");
    let t = &d.target;
    assert_eq!(decision(&d, "runtime.ruby").value, "3.3");
    assert_eq!(step_names(t), ["apt-detected", "deps-inputs", "deps", "source"]);
    assert_eq!(process(t, "web"), "bundle exec rackup -o 0.0.0.0 -p 3000");
    assert!(matches!(&t.image.runtime_steps[0], potter_plan::Step::Apt { packages, .. } if packages == &["libpq5"]));
    assert_eq!(process(t, "worker"), "bundle exec sidekiq");
}

#[test]
fn gemfile_ruby_requirement() {
    let lock = "GEM\n  remote: https://rubygems.org/\n  specs:\n    rack (3.1.0)\n";
    let r = temp_repo(&[
        (
            "Gemfile",
            "source \"https://rubygems.org\"\nruby \"~> 3.4\"\ngem \"rack\"\n",
        ),
        ("Gemfile.lock", lock),
        ("config.ru", "run ->(_) { [200, {}, []] }\n"),
    ]);
    let d = detect_at(r.path(), &DetectOptions::default()).unwrap();
    assert_eq!(decision(&d, "runtime.ruby").value, "3.4");
}

#[test]
fn rails_with_jsbundling_installs_node_first() {
    let lock = "GEM\n  specs:\n    rails (8.1.0)\n    jsbundling-rails (1.3.0)\n";
    let r = temp_repo(&[
        ("Gemfile", "gem \"rails\"\n"),
        ("Gemfile.lock", lock),
        ("package.json", r#"{"scripts":{"build":"esbuild app/javascript/*.*"}}"#),
        ("yarn.lock", "# yarn lockfile v1\n"),
    ]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(
        step_names(&t),
        [
            "deps-inputs",
            "deps",
            "node-deps-inputs",
            "node-deps",
            "source",
            "assets"
        ]
    );
    assert!(layers(&t.build.base).iter().any(|l| l.starts_with("node@")));
    assert!(!layers(&t.image.base).iter().any(|l| l.starts_with("node@")));
}

#[test]
fn missing_lockfile_and_no_process() {
    let r = temp_repo(&[("Gemfile", "gem \"rake\"\n")]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::MissingFile { .. })
    ));
    let r = temp_repo(&[
        ("Gemfile", "gem \"rake\"\n"),
        ("Gemfile.lock", "GEM\n  specs:\n    rake (13.0.0)\n"),
    ]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::NoProcess { .. })
    ));
}
