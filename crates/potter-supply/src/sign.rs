//! Signatures in the sigstore bundle v0.3 format (spec §17): ECDSA P-256 with SHA-256 under a
//! platform-held key. Statements (SBOM, provenance) are signed as DSSE envelopes; the image
//! manifest gets a message signature over its bytes. There is no transparency log in v1, so
//! `tlogEntries` is empty and trust rests on the key.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use p256::ecdsa::signature::hazmat::PrehashVerifier;
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{self, DerSignature};
use p256::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::provenance::STATEMENT_TYPE;

pub const BUNDLE_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";
pub const IN_TOTO_PAYLOAD: &str = "application/vnd.in-toto+json";

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("{0}: KMS signing keys are not supported yet; pass a PEM key file")]
    KmsUnsupported(String),
    #[error("reading the key {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Pem(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("not a sigstore bundle: {0}")]
    Malformed(String),
    #[error("the signature does not verify with this key")]
    BadSignature,
    #[error("the statement is about {found}, not {expected}")]
    SubjectMismatch { expected: String, found: String },
    #[error("expected a {expected} statement, found {found}")]
    PredicateMismatch { expected: String, found: String },
}

/// A P-256 signing key.
pub struct SigningKey(ecdsa::SigningKey);

/// A P-256 public key.
#[derive(Clone)]
pub struct VerifyingKey(ecdsa::VerifyingKey);

impl SigningKey {
    pub fn generate() -> Self {
        Self(ecdsa::SigningKey::random(&mut rand_core::OsRng))
    }

    /// A PKCS#8 (`PRIVATE KEY`) or SEC1 (`EC PRIVATE KEY`) PEM.
    pub fn from_pem(pem: &str) -> Result<Self, KeyError> {
        if let Ok(k) = ecdsa::SigningKey::from_pkcs8_pem(pem) {
            return Ok(Self(k));
        }
        p256::SecretKey::from_sec1_pem(pem)
            .map(|k| Self(k.into()))
            .map_err(|_| KeyError::Pem("not a P-256 private key in PKCS#8 or SEC1 PEM".into()))
    }

    /// `--sign-key`: a path to a PEM file; `kms://…` is refused until KMS support lands.
    pub fn load(reference: &str) -> Result<Self, KeyError> {
        if reference.starts_with("kms://") {
            return Err(KeyError::KmsUnsupported(reference.into()));
        }
        let pem = std::fs::read_to_string(Path::new(reference)).map_err(|source| KeyError::Read {
            path: reference.into(),
            source,
        })?;
        Self::from_pem(&pem)
    }

    /// PKCS#8 PEM.
    pub fn to_pem(&self) -> String {
        self.0
            .to_pkcs8_pem(LineEnding::LF)
            .expect("encoding a P-256 key")
            .to_string()
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey(*self.0.verifying_key())
    }

    fn sign(&self, message: &[u8]) -> String {
        let sig: DerSignature = self.0.sign(message);
        B64.encode(sig.as_bytes())
    }
}

impl VerifyingKey {
    /// An SPKI (`PUBLIC KEY`) PEM.
    pub fn from_pem(pem: &str) -> Result<Self, KeyError> {
        ecdsa::VerifyingKey::from_public_key_pem(pem)
            .map(Self)
            .map_err(|_| KeyError::Pem("not a P-256 public key in SPKI PEM".into()))
    }

    pub fn to_pem(&self) -> String {
        self.0.to_public_key_pem(LineEnding::LF).expect("encoding a P-256 key")
    }

    /// Base64 of the SHA-256 of the DER public key: the bundle's key hint.
    pub fn hint(&self) -> String {
        let der = self.0.to_public_key_der().expect("encoding a P-256 key");
        B64.encode(Sha256::digest(der.as_bytes()))
    }
}

/// DSSE v1 pre-authentication encoding.
fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = format!("DSSEv1 {} {payload_type} {} ", payload_type.len(), payload.len()).into_bytes();
    out.extend_from_slice(payload);
    out
}

fn bundle(key: &SigningKey, content: (&str, Value)) -> Value {
    let mut b = json!({
        "mediaType": BUNDLE_MEDIA_TYPE,
        "verificationMaterial": {
            "publicKey": {"hint": key.verifying_key().hint()},
            "tlogEntries": [],
        },
    });
    b[content.0] = content.1;
    b
}

/// A bundle holding `statement` (an in-toto statement) in a signed DSSE envelope.
pub fn sign_statement(key: &SigningKey, statement: &Value) -> Value {
    let payload = serde_json::to_vec(statement).expect("serialising a statement");
    let sig = key.sign(&pae(IN_TOTO_PAYLOAD, &payload));
    bundle(
        key,
        (
            "dsseEnvelope",
            json!({
                "payload": B64.encode(&payload),
                "payloadType": IN_TOTO_PAYLOAD,
                "signatures": [{"sig": sig, "keyid": ""}],
            }),
        ),
    )
}

/// A bundle holding a signature over the image manifest's bytes.
pub fn sign_manifest(key: &SigningKey, manifest: &[u8]) -> Value {
    bundle(
        key,
        (
            "messageSignature",
            json!({
                "messageDigest": {"algorithm": "SHA2_256", "digest": B64.encode(Sha256::digest(manifest))},
                "signature": key.sign(manifest),
            }),
        ),
    )
}

fn malformed(what: &str) -> VerifyError {
    VerifyError::Malformed(what.into())
}

fn parse_bundle(bundle: &[u8]) -> Result<Value, VerifyError> {
    let b: Value = serde_json::from_slice(bundle).map_err(|e| malformed(&e.to_string()))?;
    if b["mediaType"] != BUNDLE_MEDIA_TYPE {
        return Err(malformed("unknown mediaType"));
    }
    Ok(b)
}

fn str_field<'a>(v: &'a Value, name: &str) -> Result<&'a str, VerifyError> {
    v[name].as_str().ok_or_else(|| malformed(&format!("no {name}")))
}

fn b64_field(v: &Value, name: &str) -> Result<Vec<u8>, VerifyError> {
    B64.decode(str_field(v, name)?)
        .map_err(|_| malformed(&format!("{name} is not base64")))
}

fn der_signature(bytes: &[u8]) -> Result<ecdsa::Signature, VerifyError> {
    ecdsa::Signature::from_der(bytes).map_err(|_| VerifyError::BadSignature)
}

/// Checks a statement bundle's signature, then that the statement is a `predicate_type`
/// statement about `subject_digest` (`sha256:<hex>`). Returns the statement.
pub fn verify_statement(
    key: &VerifyingKey,
    bundle: &[u8],
    subject_digest: &str,
    predicate_type: &str,
) -> Result<Value, VerifyError> {
    let b = parse_bundle(bundle)?;
    let envelope = &b["dsseEnvelope"];
    let payload_type = str_field(envelope, "payloadType")?;
    let payload = b64_field(envelope, "payload")?;
    let signatures = envelope["signatures"]
        .as_array()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| malformed("no signatures"))?;
    let message = pae(payload_type, &payload);
    let mut verified = false;
    for s in signatures {
        if let Ok(sig) = der_signature(&b64_field(s, "sig")?) {
            verified |= key.0.verify(&message, &sig).is_ok();
        }
    }
    if !verified {
        return Err(VerifyError::BadSignature);
    }
    if payload_type != IN_TOTO_PAYLOAD {
        return Err(malformed("the payload is not an in-toto statement"));
    }
    let statement: Value = serde_json::from_slice(&payload).map_err(|e| malformed(&e.to_string()))?;
    if statement["_type"] != STATEMENT_TYPE {
        return Err(malformed("not an in-toto v1 statement"));
    }
    let found = statement["predicateType"].as_str().unwrap_or_default();
    if found != predicate_type {
        return Err(VerifyError::PredicateMismatch {
            expected: predicate_type.into(),
            found: found.into(),
        });
    }
    let (algorithm, hex) = subject_digest.split_once(':').unwrap_or(("sha256", subject_digest));
    let subjects = statement["subject"].as_array().cloned().unwrap_or_default();
    if !subjects.iter().any(|s| s["digest"][algorithm] == hex) {
        let found = subjects
            .first()
            .and_then(|s| s["digest"][algorithm].as_str())
            .map(|h| format!("{algorithm}:{h}"))
            .unwrap_or_else(|| "nothing".into());
        return Err(VerifyError::SubjectMismatch {
            expected: subject_digest.into(),
            found,
        });
    }
    Ok(statement)
}

/// Checks a manifest bundle signs the manifest with digest `manifest_digest` (`sha256:<hex>`).
pub fn verify_manifest(key: &VerifyingKey, bundle: &[u8], manifest_digest: &str) -> Result<(), VerifyError> {
    let b = parse_bundle(bundle)?;
    let m = &b["messageSignature"];
    let d = &m["messageDigest"];
    if d["algorithm"] != "SHA2_256" {
        return Err(malformed("unsupported message digest"));
    }
    let digest = b64_field(d, "digest")?;
    let signature = b64_field(m, "signature")?;
    let digest: [u8; 32] = digest.try_into().map_err(|_| malformed("bad digest length"))?;
    let sig = der_signature(&signature)?;
    // The signature covers the manifest; its prehash is the recorded digest.
    key.0
        .verify_prehash(&digest, &sig)
        .map_err(|_| VerifyError::BadSignature)?;
    let found = format!(
        "sha256:{}",
        digest.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    if found != manifest_digest {
        return Err(VerifyError::SubjectMismatch {
            expected: manifest_digest.into(),
            found,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::{SLSA_PREDICATE, SPDX_PREDICATE, in_toto};

    fn digest_of(bytes: &[u8]) -> String {
        format!("sha256:{}", hex(&Sha256::digest(bytes)))
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn bytes(v: &Value) -> Vec<u8> {
        serde_json::to_vec(v).unwrap()
    }

    #[test]
    fn keys_round_trip_through_pem() {
        let key = SigningKey::generate();
        let again = SigningKey::from_pem(&key.to_pem()).unwrap();
        assert_eq!(again.verifying_key().to_pem(), key.verifying_key().to_pem());
        let public = VerifyingKey::from_pem(&key.verifying_key().to_pem()).unwrap();
        assert_eq!(public.hint(), key.verifying_key().hint());
        assert!(key.to_pem().starts_with("-----BEGIN PRIVATE KEY-----"));
        assert!(public.to_pem().starts_with("-----BEGIN PUBLIC KEY-----"));
    }

    #[test]
    fn sec1_keys_are_accepted() {
        let generated = p256::SecretKey::random(&mut rand_core::OsRng)
            .to_sec1_pem(LineEnding::LF)
            .unwrap();
        assert!(SigningKey::from_pem(&generated).is_ok());
    }

    #[test]
    fn kms_references_are_refused() {
        assert!(matches!(
            SigningKey::load("kms://aws/alias/potter"),
            Err(KeyError::KmsUnsupported(_))
        ));
        assert!(matches!(
            SigningKey::load("/nonexistent/key.pem"),
            Err(KeyError::Read { .. })
        ));
        assert!(matches!(SigningKey::from_pem("not a key"), Err(KeyError::Pem(_))));
    }

    #[test]
    fn a_signed_statement_verifies_and_comes_back() {
        let key = SigningKey::generate();
        let statement = in_toto("img", "sha256:abcd", SLSA_PREDICATE, json!({"k": "v"}));
        let bundle = sign_statement(&key, &statement);
        assert_eq!(bundle["mediaType"], BUNDLE_MEDIA_TYPE);
        assert_eq!(bundle["dsseEnvelope"]["payloadType"], IN_TOTO_PAYLOAD);
        assert_eq!(
            bundle["verificationMaterial"]["publicKey"]["hint"],
            key.verifying_key().hint()
        );
        assert_eq!(bundle["verificationMaterial"]["tlogEntries"], json!([]));
        let got = verify_statement(&key.verifying_key(), &bytes(&bundle), "sha256:abcd", SLSA_PREDICATE).unwrap();
        assert_eq!(got, statement);
    }

    #[test]
    fn a_statement_fails_under_another_key_or_when_tampered() {
        let key = SigningKey::generate();
        let statement = in_toto("img", "sha256:abcd", SLSA_PREDICATE, json!({}));
        let bundle = sign_statement(&key, &statement);
        let other = SigningKey::generate().verifying_key();
        assert_eq!(
            verify_statement(&other, &bytes(&bundle), "sha256:abcd", SLSA_PREDICATE),
            Err(VerifyError::BadSignature)
        );
        let mut tampered = bundle.clone();
        let forged = in_toto("img", "sha256:ffff", SLSA_PREDICATE, json!({}));
        tampered["dsseEnvelope"]["payload"] = json!(B64.encode(bytes(&forged)));
        assert_eq!(
            verify_statement(&key.verifying_key(), &bytes(&tampered), "sha256:ffff", SLSA_PREDICATE),
            Err(VerifyError::BadSignature)
        );
        // The payload type is covered by the signature too.
        let mut retyped = bundle.clone();
        retyped["dsseEnvelope"]["payloadType"] = json!("text/plain");
        assert!(verify_statement(&key.verifying_key(), &bytes(&retyped), "sha256:abcd", SLSA_PREDICATE).is_err());
    }

    #[test]
    fn a_statement_about_another_image_or_kind_is_refused() {
        let key = SigningKey::generate();
        let bundle = bytes(&sign_statement(
            &key,
            &in_toto("img", "sha256:abcd", SLSA_PREDICATE, json!({})),
        ));
        assert_eq!(
            verify_statement(&key.verifying_key(), &bundle, "sha256:0000", SLSA_PREDICATE),
            Err(VerifyError::SubjectMismatch {
                expected: "sha256:0000".into(),
                found: "sha256:abcd".into()
            })
        );
        assert!(matches!(
            verify_statement(&key.verifying_key(), &bundle, "sha256:abcd", SPDX_PREDICATE),
            Err(VerifyError::PredicateMismatch { .. })
        ));
    }

    #[test]
    fn a_manifest_signature_binds_the_digest() {
        let key = SigningKey::generate();
        let manifest = br#"{"schemaVersion":2}"#;
        let bundle = sign_manifest(&key, manifest);
        assert_eq!(bundle["mediaType"], BUNDLE_MEDIA_TYPE);
        assert_eq!(bundle["messageSignature"]["messageDigest"]["algorithm"], "SHA2_256");
        let digest = digest_of(manifest);
        verify_manifest(&key.verifying_key(), &bytes(&bundle), &digest).unwrap();
        assert_eq!(
            verify_manifest(&key.verifying_key(), &bytes(&bundle), &digest_of(b"other")),
            Err(VerifyError::SubjectMismatch {
                expected: digest_of(b"other"),
                found: digest.clone()
            })
        );
        assert_eq!(
            verify_manifest(&SigningKey::generate().verifying_key(), &bytes(&bundle), &digest),
            Err(VerifyError::BadSignature)
        );
        // A forged digest field with the original signature does not verify.
        let mut forged = bundle.clone();
        forged["messageSignature"]["messageDigest"]["digest"] = json!(B64.encode(Sha256::digest(b"other")));
        assert_eq!(
            verify_manifest(&key.verifying_key(), &bytes(&forged), &digest_of(b"other")),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn garbage_is_malformed_not_a_panic() {
        let key = SigningKey::generate().verifying_key();
        for junk in [&b"{"[..], b"{}", br#"{"mediaType":"x"}"#, br#"{"mediaType":"application/vnd.dev.sigstore.bundle.v0.3+json","dsseEnvelope":{"payload":"!!","payloadType":"application/vnd.in-toto+json","signatures":[]}}"#] {
            assert!(matches!(
                verify_statement(&key, junk, "sha256:abcd", SLSA_PREDICATE),
                Err(VerifyError::Malformed(_))
            ));
            assert!(matches!(verify_manifest(&key, junk, "sha256:abcd"), Err(VerifyError::Malformed(_))));
        }
    }
}
