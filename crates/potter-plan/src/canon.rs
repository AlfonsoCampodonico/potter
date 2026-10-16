//! RFC 8785 (JCS) canonical JSON and SHA-256 hashing (spec §5.2).

use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::error::PlanError;
use crate::ir::{Plan, Step};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest(pub [u8; 32]);

impl Sha256Digest {
    pub fn of(bytes: &[u8]) -> Self {
        let out = Sha256::digest(bytes);
        let mut a = [0u8; 32];
        a.copy_from_slice(out.as_slice());
        Sha256Digest(a)
    }

    pub fn hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0xf) as usize] as char);
        }
        s
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<String, PlanError> {
    let v = serde_json::to_value(value)?;
    let mut out = String::new();
    write_value(&v, &mut out)?;
    Ok(out)
}

fn write_value(v: &Value, out: &mut String) -> Result<(), PlanError> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                return Err(PlanError::Float);
            }
            out.push_str(&n.to_string());
        }
        // serde_json escapes exactly what ECMAScript JSON.stringify escapes: `"`, `\`, and C0
        // controls (as \b \f \n \r \t or lower-case \u00xx). Everything else is literal UTF-8.
        Value::String(s) => out.push_str(&serde_json::to_string(s)?),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k)?);
                out.push(':');
                write_value(&map[k.as_str()], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

pub fn hash_of<T: Serialize>(value: &T) -> Result<Sha256Digest, PlanError> {
    Ok(Sha256Digest::of(canonical_json(value)?.as_bytes()))
}

/// The hash of a step for cache keys (spec §8.1): labels and timeouts are execution-only.
pub fn step_hash(step: &Step) -> Result<Sha256Digest, PlanError> {
    let mut v = serde_json::to_value(step)?;
    if let Value::Object(m) = &mut v {
        m.remove("name");
        m.remove("timeout_secs");
    }
    let mut out = String::new();
    write_value(&v, &mut out)?;
    Ok(Sha256Digest::of(out.as_bytes()))
}

/// The hash of a whole plan, used by `--affected` (spec §10). Timeouts don't count.
pub fn plan_hash(plan: &Plan) -> Result<Sha256Digest, PlanError> {
    let mut p = plan.clone();
    for t in &mut p.targets {
        for s in t.build.steps.iter_mut().chain(t.image.runtime_steps.iter_mut()) {
            if let Step::Run { timeout_secs, .. } = s {
                *timeout_secs = None;
            }
        }
    }
    hash_of(&p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::*;
    use crate::testing::sample_plan;
    use serde_json::json;

    #[test]
    fn keys_sorted_no_whitespace() {
        assert_eq!(
            canonical_json(&json!({"b": 2, "a": [1, {"d": null, "c": true}]})).unwrap(),
            r#"{"a":[1,{"c":true,"d":null}],"b":2}"#
        );
    }

    #[test]
    fn keys_sort_by_utf16_code_units() {
        // U+10000 is D800 DC00 in UTF-16, which sorts before U+E000, unlike in UTF-8.
        let v = json!({"\u{e000}": 1, "\u{10000}": 2});
        assert_eq!(canonical_json(&v).unwrap(), "{\"\u{10000}\":2,\"\u{e000}\":1}");
    }

    #[test]
    fn string_escaping_matches_jcs() {
        assert_eq!(
            canonical_json(&json!("a\u{1}\"\\/é\n")).unwrap(),
            "\"a\\u0001\\\"\\\\/é\\n\""
        );
    }

    #[test]
    fn floats_are_rejected() {
        assert!(matches!(canonical_json(&json!(1.5)), Err(PlanError::Float)));
    }

    #[test]
    fn step_hash_ignores_name_and_timeout() {
        let plan = sample_plan();
        let step = plan.targets[0].build.steps[1].clone();
        let mut renamed = step.clone();
        if let Step::Run { name, timeout_secs, .. } = &mut renamed {
            *name = "other".into();
            *timeout_secs = Some(60);
        }
        assert_eq!(step_hash(&step).unwrap(), step_hash(&renamed).unwrap());
        let mut changed = step.clone();
        if let Step::Run { command, .. } = &mut changed {
            *command = Command::shell("composer install");
        }
        assert_ne!(step_hash(&step).unwrap(), step_hash(&changed).unwrap());
    }

    #[test]
    fn plan_hash_ignores_timeouts_only() {
        let plan = sample_plan();
        let mut timed = plan.clone();
        if let Step::Run { timeout_secs, .. } = &mut timed.targets[0].build.steps[1] {
            *timeout_secs = Some(30);
        }
        assert_eq!(plan_hash(&plan).unwrap(), plan_hash(&timed).unwrap());
        let mut renamed = plan.clone();
        renamed.targets[0].name = "web".into();
        assert_ne!(plan_hash(&plan).unwrap(), plan_hash(&renamed).unwrap());
    }

    #[test]
    fn an_unset_layer_budget_is_absent_from_the_canonical_form() {
        let plan = sample_plan();
        let json = canonical_json(&plan).unwrap();
        assert!(!json.contains("max_layers"), "plans without a budget hash as before");
        let mut budgeted = plan.clone();
        budgeted.targets[0].image.max_layers = Some(20);
        assert!(canonical_json(&budgeted).unwrap().contains(r#""max_layers":20"#));
        assert_ne!(
            hash_of(&plan.targets[0]).unwrap(),
            hash_of(&budgeted.targets[0]).unwrap(),
            "the image changes, so the target's hash does"
        );
        // Step hashes (and so step cache keys) never see the image's budget.
        let s = &plan.targets[0].build.steps[1];
        assert_eq!(
            step_hash(s).unwrap(),
            step_hash(&budgeted.targets[0].build.steps[1]).unwrap()
        );
    }

    #[test]
    fn digest_display() {
        let d = Sha256Digest::of(b"");
        assert_eq!(
            d.to_string(),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
