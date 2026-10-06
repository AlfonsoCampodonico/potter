//! The published JSON Schema for `potter.plan/v1`.

use crate::ir::Plan;

pub fn json_schema() -> String {
    let schema = schemars::schema_for!(Plan);
    let mut s = serde_json::to_string_pretty(&schema).expect("a schema always serialises");
    s.push('\n');
    s
}
