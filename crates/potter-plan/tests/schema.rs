//! The committed JSON Schema must match the types. Regenerate with `UPDATE_SCHEMA=1 cargo test -p potter-plan`.

use std::path::Path;

#[test]
fn committed_schema_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schema/potter.plan.v1.json");
    let generated = potter_plan::json_schema();
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
    }
    let committed =
        std::fs::read_to_string(&path).expect("docs/schema/potter.plan.v1.json exists; run with UPDATE_SCHEMA=1");
    assert_eq!(
        committed, generated,
        "schema drifted; run UPDATE_SCHEMA=1 cargo test -p potter-plan"
    );
}
