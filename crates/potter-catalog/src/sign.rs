//! ed25519 signatures over an index blob's SHA-256, and the keys that are trusted to make them.

use base64::{Engine, engine::general_purpose::STANDARD};
pub use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
use ed25519_dalek::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::{Signature, Signer, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::CatalogError;

include!(concat!(env!("OUT_DIR"), "/compiled_keys.rs"));

/// A public key potter accepts catalog signatures from.
#[derive(Debug, Clone)]
pub struct TrustedKey {
    /// First 16 hex digits of the SHA-256 of the raw public key.
    pub id: String,
    pub key: VerifyingKey,
}

impl TrustedKey {
    pub fn new(key: VerifyingKey) -> Self {
        Self { id: key_id(&key), key }
    }

    pub fn from_pem(pem: &str) -> Result<Self, String> {
        VerifyingKey::from_public_key_pem(pem)
            .map(Self::new)
            .map_err(|e| format!("not an ed25519 public key in PEM: {e}"))
    }
}

/// The key id: first 16 hex digits of the SHA-256 of the raw 32-byte public key.
pub fn key_id(key: &VerifyingKey) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every `keys/*.pub` compiled into the binary. A key that fails to parse is a build mistake
/// and is skipped here; the release check in CI catches it.
pub fn compiled_keys() -> Vec<TrustedKey> {
    COMPILED_KEYS
        .iter()
        .filter_map(|pem| TrustedKey::from_pem(pem).ok())
        .collect()
}

/// The compiled keys plus the one named by `POTTER_CATALOG_KEY` (a path to a public-key PEM).
pub fn trusted_keys() -> Result<Vec<TrustedKey>, CatalogError> {
    trusted_keys_and(&[])
}

/// [`trusted_keys`] and `extra`, keys the process made itself (the catalog driver's ephemeral
/// key). With `extra` the `POTTER_CATALOG_KEY` note is not printed again: a process that makes
/// its own keys loads catalogs many times, and notes the variable on its first plain load.
pub(crate) fn trusted_keys_and(extra: &[TrustedKey]) -> Result<Vec<TrustedKey>, CatalogError> {
    let (mut keys, notes) = trusted_keys_with(std::env::var("POTTER_CATALOG_KEY").ok().as_deref())?;
    if extra.is_empty() {
        for note in notes {
            eprintln!("{note}");
        }
    }
    keys.extend(extra.iter().cloned());
    Ok(keys)
}

/// The trusted keys and the `note:` lines the caller should print.
pub(crate) fn trusted_keys_with(extra: Option<&str>) -> Result<(Vec<TrustedKey>, Vec<String>), CatalogError> {
    let mut keys = compiled_keys();
    let mut notes = Vec::new();
    if let Some(path) = extra.filter(|p| !p.is_empty()) {
        let pem =
            std::fs::read_to_string(path).map_err(|e| CatalogError::Plan(format!("POTTER_CATALOG_KEY {path}: {e}")))?;
        let key =
            TrustedKey::from_pem(&pem).map_err(|e| CatalogError::Plan(format!("POTTER_CATALOG_KEY {path}: {e}")))?;
        notes.push(format!(
            "note: trusting the extra catalog key {} from POTTER_CATALOG_KEY",
            key.id
        ));
        keys.push(key);
    }
    Ok((keys, notes))
}

/// Signs `blob_sha256`; returns the key id and the base64 signature.
pub fn sign(blob_sha256: &[u8; 32], key: &SigningKey) -> (String, String) {
    let sig = key.sign(blob_sha256);
    (key_id(&key.verifying_key()), STANDARD.encode(sig.to_bytes()))
}

/// Checks `sig_b64` over `blob_sha256` against the trusted key named `key_id`.
pub fn verify(blob_sha256: &[u8; 32], key_id: &str, sig_b64: &str, keys: &[TrustedKey]) -> Result<(), CatalogError> {
    let hostile = |why: String| CatalogError::Hostile(why);
    let trusted = keys
        .iter()
        .find(|k| k.id == key_id)
        .ok_or_else(|| hostile(format!("the index is signed by key {key_id}, which is not trusted")))?;
    let raw = STANDARD
        .decode(sig_b64)
        .map_err(|e| hostile(format!("the index signature is not base64: {e}")))?;
    let sig = Signature::from_slice(&raw).map_err(|e| hostile(format!("the index signature is malformed: {e}")))?;
    trusted
        .key
        .verify_strict(blob_sha256, &sig)
        .map_err(|_| hostile(format!("the index signature does not verify under key {key_id}")))
}

/// The ed25519 signing key in a PKCS#8 PEM, as `potter key generate --ed25519` writes it.
pub fn signing_key_from_pem(pem: &str) -> Result<SigningKey, String> {
    SigningKey::from_pkcs8_pem(pem).map_err(|e| format!("not an ed25519 private key in PKCS#8 PEM: {e}"))
}

/// A new ed25519 key pair as PEM: (PKCS#8 private, SPKI public).
pub fn generate_pem() -> (String, String) {
    let key = SigningKey::generate(&mut rand_core::OsRng);
    let private = key
        .to_pkcs8_pem(LineEnding::LF)
        .expect("an ed25519 key encodes")
        .to_string();
    let public = key
        .verifying_key()
        .to_public_key_pem(LineEnding::LF)
        .expect("an ed25519 key encodes");
    (private, public)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (SigningKey, TrustedKey) {
        let sk = SigningKey::generate(&mut rand_core::OsRng);
        let tk = TrustedKey::new(sk.verifying_key());
        (sk, tk)
    }

    #[test]
    fn sign_then_verify() {
        let (sk, tk) = pair();
        let blob = [7u8; 32];
        let (id, sig) = sign(&blob, &sk);
        assert_eq!(id, tk.id);
        assert_eq!(id.len(), 16);
        verify(&blob, &id, &sig, &[tk]).unwrap();
    }

    #[test]
    fn wrong_key_is_hostile() {
        let (sk, _) = pair();
        let (_, other) = pair();
        let blob = [7u8; 32];
        let (_, sig) = sign(&blob, &sk);
        // Same id claimed, different key behind it.
        let id = other.id.clone();
        let err = verify(&blob, &id, &sig, &[other]).unwrap_err();
        assert!(matches!(err, CatalogError::Hostile(_)), "{err}");
    }

    #[test]
    fn unknown_key_id_is_hostile() {
        let (sk, _) = pair();
        let (_, other) = pair();
        let blob = [7u8; 32];
        let (id, sig) = sign(&blob, &sk);
        let err = verify(&blob, &id, &sig, &[other]).unwrap_err();
        assert!(
            matches!(&err, CatalogError::Hostile(m) if m.contains("not trusted")),
            "{err}"
        );
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn flipped_bit_is_hostile() {
        let (sk, tk) = pair();
        let blob = [7u8; 32];
        let (id, sig) = sign(&blob, &sk);
        let mut other = blob;
        other[0] ^= 1;
        let err = verify(&other, &id, &sig, std::slice::from_ref(&tk)).unwrap_err();
        assert!(matches!(err, CatalogError::Hostile(_)));
        let mut raw = STANDARD.decode(&sig).unwrap();
        raw[3] ^= 1;
        let err = verify(&blob, &id, &STANDARD.encode(raw), &[tk]).unwrap_err();
        assert!(matches!(err, CatalogError::Hostile(_)));
    }

    #[test]
    fn every_key_file_is_compiled() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("keys");
        let files = std::fs::read_dir(dir)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "pub"))
            .count();
        assert_eq!(COMPILED_KEYS.len(), files);
        assert_eq!(compiled_keys().len(), files);
    }

    #[test]
    fn env_key_is_trusted_and_noted() {
        let (_, public) = generate_pem();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("extra.pub");
        std::fs::write(&path, &public).unwrap();
        let expected = TrustedKey::from_pem(&public).unwrap();
        let (keys, notes) = trusted_keys_with(Some(path.to_str().unwrap())).unwrap();
        assert!(keys.iter().any(|k| k.id == expected.id));
        assert_eq!(keys.len(), compiled_keys().len() + 1);
        assert_eq!(
            notes,
            vec![format!(
                "note: trusting the extra catalog key {} from POTTER_CATALOG_KEY",
                expected.id
            )]
        );
        // A missing or malformed file is the caller's mistake.
        let bad = trusted_keys_with(Some("/nonexistent/key.pub")).unwrap_err();
        assert!(matches!(bad, CatalogError::Plan(_)));
        std::fs::write(&path, "nope").unwrap();
        assert!(matches!(
            trusted_keys_with(path.to_str()).unwrap_err(),
            CatalogError::Plan(_)
        ));
        // No variable: just the compiled keys, and nothing to say.
        let (keys, notes) = trusted_keys_with(None).unwrap();
        assert_eq!(keys.len(), compiled_keys().len());
        assert!(notes.is_empty());

        // trusted_keys() reads the variable. This is the only test that touches the process env.
        std::fs::write(&path, &public).unwrap();
        // SAFETY: no other test in this crate reads or writes POTTER_CATALOG_KEY.
        unsafe { std::env::set_var("POTTER_CATALOG_KEY", &path) };
        let keys = trusted_keys().unwrap();
        unsafe { std::env::remove_var("POTTER_CATALOG_KEY") };
        assert!(keys.iter().any(|k| k.id == expected.id));
    }
}
