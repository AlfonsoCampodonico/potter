//! Lists `keys/*.pub` so `sign::compiled_keys` can `include_str!` each one, and refuses to build
//! when any of them is not an ed25519 public-key PEM.

use std::path::PathBuf;

use ed25519_dalek::VerifyingKey;
use ed25519_dalek::pkcs8::DecodePublicKey;

fn main() {
    println!("cargo:rerun-if-changed=keys");
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("keys");
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.unwrap_or_else(|e| panic!("{}: {e}", dir.display())).path();
        if path.extension().is_some_and(|x| x == "pub") {
            files.push(path);
        }
    }
    files.sort();
    let mut out = String::from("pub const COMPILED_KEYS: &[&str] = &[\n");
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let pem = std::fs::read_to_string(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        if let Err(e) = VerifyingKey::from_public_key_pem(&pem) {
            panic!("{} is not an ed25519 public key in PEM: {e}", f.display());
        }
        out.push_str(&format!("    include_str!({:?}),\n", f.display().to_string()));
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("compiled_keys.rs");
    std::fs::write(dest, out).unwrap();
}
