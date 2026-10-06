//! One snapshot per fixture: the canonical plan (pretty-printed) and every decision, or the error.
//! Review changes with `cargo insta review`, or regenerate with `INSTA_UPDATE=always`.

use std::fmt::Write;
use std::path::Path;

use potter_config::plan_repo;

fn render(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name);
    match plan_repo(&path, None) {
        Ok(p) => {
            let mut s = serde_json::to_string_pretty(&p.plan).unwrap();
            s.push_str("\n\n# decisions\n");
            for (target, ds) in &p.decisions {
                for d in ds {
                    writeln!(s, "{target} {} = {} [{}]", d.field, d.value, d.origin).unwrap();
                }
            }
            s
        }
        Err(e) => format!("error: {e}\n"),
    }
}

macro_rules! fixtures {
    ($($name:ident => $dir:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                insta::assert_snapshot!(render($dir));
            }
        )*
    };
}

fixtures! {
    php_composer => "php-composer",
    drupal => "drupal",
    laravel => "laravel",
    laravel_octane => "laravel-octane",
    laravel_horizon_pnpm => "laravel-horizon-pnpm",
    nestjs => "nestjs",
    next_standalone => "next-standalone",
    nuxt => "nuxt",
    hono_node => "hono-node",
    vue_node => "vue-node",
    bun => "bun",
    deno_serve => "deno-serve",
    deno_fresh => "deno-fresh",
    vite_static => "vite-static",
    overrides => "overrides",
    explicit_go => "explicit-go",
    ambiguous => "ambiguous",
    monorepo_root => "monorepo-root",
    monorepo_targets => "monorepo-targets",
    python_django => "python-django",
    python_fastapi => "python-fastapi",
    python_flask => "python-flask",
    python_uv => "python-uv",
    python_poetry => "python-poetry",
    ruby_rails => "ruby-rails",
    ruby_sinatra => "ruby-sinatra",
    go_api => "go-api",
    go_cmd => "go-cmd",
    rust_axum => "rust-axum",
    rust_workspace => "rust-workspace",
}
