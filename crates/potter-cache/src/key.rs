//! Cache keys (spec §8.1).

use std::collections::BTreeMap;

use potter_plan::Step;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::CacheError;

/// Bumped only when step-execution semantics change; it invalidates every key.
pub const POTTER_MAJOR: &str = "1";

/// Who executed a step (spec §8.4): VM outputs never mix with namespace outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutorClass {
    Vm,
    Ns,
}

impl ExecutorClass {
    /// The entry classes a build of this class looks up, most preferred first (spec §8.4): a
    /// `vm` build uses `vm` keys only; an `ns` build tries the `vm` key when its own misses.
    pub fn lookup_order(self) -> &'static [ExecutorClass] {
        match self {
            ExecutorClass::Vm => &[ExecutorClass::Vm],
            ExecutorClass::Ns => &[ExecutorClass::Ns, ExecutorClass::Vm],
        }
    }

    /// Whether a build of this class may use an entry of class `entry` (P3).
    pub fn accepts(self, entry: ExecutorClass) -> bool {
        self.lookup_order().contains(&entry)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ExecutorClass::Vm => "vm",
            ExecutorClass::Ns => "ns",
        }
    }
}

/// A step's cache key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey(pub [u8; 32]);

impl CacheKey {
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Everything besides the parent that a step's key covers.
pub struct StepInput<'a> {
    pub step: &'a Step,
    /// The digest of the host-built layer of an `Inputs` step.
    pub inputs_digest: Option<&'a str>,
    /// The effective environment when the step runs.
    pub env: &'a BTreeMap<String, String>,
    /// e.g. `linux/arm64`.
    pub platform: &'a str,
    pub class: ExecutorClass,
    /// [`secrets_digest`] of the secrets the step mounts (empty without any).
    pub secrets: &'a [u8],
}

/// The key material of a step's secrets (spec §5.4): `HMAC-SHA256(tenant_key, id ‖ 0 ‖ value)`
/// per secret, in id order. The values themselves never enter a key.
pub fn secrets_digest(tenant_key: &[u8; 32], secrets: &[(String, Vec<u8>)]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    let mut sorted: Vec<&(String, Vec<u8>)> = secrets.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (id, value) in sorted {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(tenant_key).expect("HMAC takes any key length");
        mac.update(id.as_bytes());
        mac.update(&[0]);
        mac.update(value);
        out.extend_from_slice(&mac.finalize().into_bytes());
    }
    out
}

/// SHA-256 over `domain`, then each field as a big-endian u64 length and its bytes.
fn hash(domain: &str, fields: &[&[u8]]) -> CacheKey {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    for f in fields {
        h.update((f.len() as u64).to_be_bytes());
        h.update(f);
    }
    CacheKey(h.finalize().into())
}

/// The key a chain starts from: its base image's manifest digest.
pub fn base_key(base_digest: &str) -> CacheKey {
    hash("potter.cache/v1/base", &[base_digest.as_bytes()])
}

/// The key of a step whose parent (the previous step, or the base) has key `parent`.
pub fn step_key(parent: &CacheKey, input: &StepInput) -> Result<CacheKey, CacheError> {
    let step = potter_plan::step_hash(input.step).map_err(CacheError::Plan)?;
    let env = potter_plan::hash_of(input.env).map_err(CacheError::Plan)?;
    Ok(hash(
        "potter.cache/v1",
        &[
            &parent.0,
            &step.0,
            input.inputs_digest.unwrap_or_default().as_bytes(),
            &env.0,
            input.secrets,
            input.platform.as_bytes(),
            input.class.as_str().as_bytes(),
            POTTER_MAJOR.as_bytes(),
        ],
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use potter_plan::{Command, Step};

    use super::*;

    fn run(script: &str) -> Step {
        Step::Run {
            name: "build".into(),
            command: Command::shell(script),
            env: BTreeMap::new(),
            workdir: "/app".into(),
            user: "0:0".into(),
            network: true,
            cache_mounts: vec![],
            secrets: vec![],
            timeout_secs: None,
        }
    }

    fn input<'a>(step: &'a Step, env: &'a BTreeMap<String, String>) -> StepInput<'a> {
        StepInput {
            step,
            inputs_digest: None,
            env,
            platform: "linux/arm64",
            class: ExecutorClass::Ns,
            secrets: &[],
        }
    }

    #[test]
    fn every_field_changes_the_key_and_nothing_else_does() {
        let env = BTreeMap::new();
        let parent = base_key("sha256:aa");
        let step = run("make");
        let k = step_key(&parent, &input(&step, &env)).unwrap();
        assert_eq!(k, step_key(&parent, &input(&step, &env)).unwrap(), "stable");
        let mut renamed = step.clone();
        if let Step::Run { name, timeout_secs, .. } = &mut renamed {
            *name = "other".into();
            *timeout_secs = Some(5);
        }
        assert_eq!(
            k,
            step_key(&parent, &input(&renamed, &env)).unwrap(),
            "name and timeout"
        );
        assert_ne!(
            k,
            step_key(&base_key("sha256:bb"), &input(&step, &env)).unwrap(),
            "parent"
        );
        assert_ne!(k, step_key(&parent, &input(&run("make all"), &env)).unwrap(), "command");
        let env2: BTreeMap<_, _> = [("A".to_string(), "1".to_string())].into();
        assert_ne!(k, step_key(&parent, &input(&step, &env2)).unwrap(), "env");
        let mut i = input(&step, &env);
        i.inputs_digest = Some("sha256:cc");
        assert_ne!(k, step_key(&parent, &i).unwrap(), "inputs");
        let mut i = input(&step, &env);
        i.platform = "linux/amd64";
        assert_ne!(k, step_key(&parent, &i).unwrap(), "platform");
        let mut i = input(&step, &env);
        i.class = ExecutorClass::Vm;
        assert_ne!(k, step_key(&parent, &i).unwrap(), "class");
    }

    #[test]
    fn a_vm_build_looks_up_vm_keys_only_and_an_ns_build_falls_back_to_vm() {
        assert_eq!(ExecutorClass::Vm.lookup_order(), [ExecutorClass::Vm]);
        assert_eq!(ExecutorClass::Ns.lookup_order(), [ExecutorClass::Ns, ExecutorClass::Vm]);
        assert!(ExecutorClass::Ns.accepts(ExecutorClass::Vm));
        assert!(ExecutorClass::Ns.accepts(ExecutorClass::Ns));
        assert!(ExecutorClass::Vm.accepts(ExecutorClass::Vm));
        assert!(
            !ExecutorClass::Vm.accepts(ExecutorClass::Ns),
            "P3: the reverse is never done"
        );
    }

    #[test]
    fn secrets_enter_the_key_only_as_a_tenant_keyed_hmac() {
        let env = BTreeMap::new();
        let step = run("make");
        let parent = base_key("sha256:aa");
        let none = step_key(&parent, &input(&step, &env)).unwrap();
        let k1 = [1u8; 32];
        let a = secrets_digest(&k1, &[("npm".into(), b"token-a".to_vec())]);
        let b = secrets_digest(&k1, &[("npm".into(), b"token-b".to_vec())]);
        let other_tenant = secrets_digest(&[2u8; 32], &[("npm".into(), b"token-a".to_vec())]);
        assert_ne!(a, b);
        assert_ne!(a, other_tenant);
        assert!(
            !a.windows(7).any(|w| w == b"token-a"),
            "the value itself never enters the key material"
        );
        // order of ids does not matter
        let ab = secrets_digest(&k1, &[("a".into(), b"1".to_vec()), ("b".into(), b"2".to_vec())]);
        let ba = secrets_digest(&k1, &[("b".into(), b"2".to_vec()), ("a".into(), b"1".to_vec())]);
        assert_eq!(ab, ba);
        let mut i = input(&step, &env);
        i.secrets = &a;
        let with_a = step_key(&parent, &i).unwrap();
        i.secrets = &b;
        assert_ne!(with_a, step_key(&parent, &i).unwrap());
        assert_ne!(with_a, none);
        assert!(secrets_digest(&k1, &[]).is_empty());
    }

    #[test]
    fn length_prefixing_keeps_fields_apart() {
        let env = BTreeMap::new();
        let step = run("x");
        let mut a = input(&step, &env);
        a.inputs_digest = Some("ab");
        a.platform = "c";
        let mut b = input(&step, &env);
        b.inputs_digest = Some("a");
        b.platform = "bc";
        assert_ne!(
            step_key(&base_key("p"), &a).unwrap(),
            step_key(&base_key("p"), &b).unwrap()
        );
    }

    #[test]
    fn keys_print_as_lowercase_hex() {
        let k = base_key("sha256:aa");
        assert_eq!(k.hex().len(), 64);
        assert!(
            k.hex()
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }
}
