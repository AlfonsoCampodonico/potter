mod common;

use common::*;
use potter_detect::{DetectError, DetectOptions, PythonOptions};

#[test]
fn django_with_requirements() {
    let d = detect("python-django");
    let t = &d.target;
    assert_eq!(t.provider, "python");
    assert_eq!(layers(&t.build.base), ["os-min", "python@3.13"]);
    assert_eq!(step_names(t), ["deps-inputs", "deps", "source", "collectstatic"]);
    assert_eq!(includes(t, "deps-inputs"), ["requirements.txt"]);
    assert_eq!(
        command(t, "deps"),
        "python -m venv /app/.venv && /app/.venv/bin/pip install --no-input -r requirements.txt"
    );
    assert_eq!(
        command(t, "collectstatic"),
        "/app/.venv/bin/python manage.py collectstatic --noinput"
    );
    assert_eq!(
        process(t, "web"),
        "/app/.venv/bin/gunicorn config.wsgi --bind 0.0.0.0:8000"
    );
    assert_eq!(t.image.config.port, Some(8000));
    assert_eq!(t.image.config.env["VIRTUAL_ENV"], "/app/.venv");
    assert_eq!(layers(&t.image.base), ["os-min", "python@3.13"]);
    potter_plan::validate(&potter_plan::Plan::new(vec![t.clone()])).unwrap();
}

#[test]
fn collectstatic_can_be_disabled() {
    let opts = DetectOptions {
        python: PythonOptions {
            collectstatic: Some(false),
            ..Default::default()
        },
        ..Default::default()
    };
    let t = detect_at(&fixture("python-django"), &opts).unwrap().target;
    assert!(!step_names(&t).contains(&"collectstatic"));
}

#[test]
fn fastapi_reexported_app() {
    let t = detect("python-fastapi").target;
    assert_eq!(
        process(&t, "web"),
        "/app/.venv/bin/uvicorn main:app --host 0.0.0.0 --port 8000"
    );
}

#[test]
fn flask_in_a_package() {
    let t = detect("python-flask").target;
    assert_eq!(
        process(&t, "web"),
        "/app/.venv/bin/gunicorn app.main:app --bind 0.0.0.0:8000"
    );
}

#[test]
fn uv_project_with_native_deps() {
    let d = detect("python-uv");
    let t = &d.target;
    assert_eq!(layers(&t.build.base), ["os-min", "python@3.12", "uv"]);
    assert_eq!(
        step_names(t),
        ["apt-detected", "deps-inputs", "deps", "source", "project"]
    );
    assert_eq!(includes(t, "deps-inputs"), ["pyproject.toml", "uv.lock"]);
    assert_eq!(command(t, "deps"), "uv sync --frozen --no-dev --no-install-project");
    assert_eq!(command(t, "project"), "uv sync --frozen --no-dev");
    assert!(matches!(&t.image.runtime_steps[0], potter_plan::Step::Apt { packages, .. } if packages == &["libpq5"]));
    assert_eq!(
        process(t, "web"),
        "/app/.venv/bin/uvicorn app:app --host 0.0.0.0 --port 8000"
    );
    assert_eq!(decision(&d, "runtime.python").value, "3.12");
}

#[test]
fn poetry_project() {
    let d = detect("python-poetry");
    let t = &d.target;
    assert_eq!(layers(&t.build.base), ["os-min", "python@3.14", "poetry"]);
    assert_eq!(
        command(t, "deps"),
        "poetry install --only main --no-root --no-interaction"
    );
    assert_eq!(
        process(t, "web"),
        "/app/.venv/bin/gunicorn wsgi:app --bind 0.0.0.0:8000"
    );
    assert!(
        decision(&d, "runtime.python")
            .origin
            .to_string()
            .contains("tool.poetry.dependencies.python")
    );
}

#[test]
fn requirement_includes_become_inputs() {
    let r = temp_repo(&[
        ("requirements.txt", "-r requirements/base.txt\nflask==3.1\n"),
        (
            "requirements/base.txt",
            "gunicorn==23.0.0  # server\n-c constraints.txt\n",
        ),
        ("requirements/constraints.txt", "werkzeug<4\n"),
        ("app.py", "from flask import Flask\napp = Flask(__name__)\n"),
    ]);
    let t = detect_at(r.path(), &DetectOptions::default()).unwrap().target;
    assert_eq!(
        includes(&t, "deps-inputs"),
        [
            "requirements.txt",
            "requirements/base.txt",
            "requirements/constraints.txt"
        ]
    );
    assert_eq!(
        process(&t, "web"),
        "/app/.venv/bin/gunicorn app:app --bind 0.0.0.0:8000"
    );
}

#[test]
fn escaping_include_is_refused() {
    let r = temp_repo(&[("requirements.txt", "-r ../../etc/passwd\n")]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::Parse { .. })
    ));
}

#[test]
fn missing_lock_and_missing_server() {
    let r = temp_repo(&[("pyproject.toml", "[project]\nname = \"x\"\n")]);
    assert!(matches!(
        detect_at(r.path(), &DetectOptions::default()),
        Err(DetectError::MissingFile { .. })
    ));
    let r = temp_repo(&[("requirements.txt", "fastapi\n"), ("main.py", "app = FastAPI()\n")]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(e.to_string().contains("uvicorn or granian"), "{e}");
}

#[test]
fn version_sources() {
    let r = temp_repo(&[
        ("requirements.txt", "flask\ngunicorn\n"),
        ("app.py", "app = Flask(__name__)\n"),
        ("runtime.txt", "python-3.12.7\n"),
    ]);
    assert_eq!(
        decision(
            &detect_at(r.path(), &DetectOptions::default()).unwrap(),
            "runtime.python"
        )
        .value,
        "3.12"
    );
    let r = temp_repo(&[
        ("requirements.txt", "flask\ngunicorn\n"),
        ("app.py", "app = Flask(__name__)\n"),
        (".python-version", "3.10\n"),
    ]);
    let e = detect_at(r.path(), &DetectOptions::default()).unwrap_err();
    assert!(e.to_string().contains("available: 3.12, 3.13, 3.14"), "{e}");
}
