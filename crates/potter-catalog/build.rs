//! Lists `keys/*.pub` so `sign::compiled_keys` can `include_str!` each one.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=keys");
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("keys");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "pub"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let mut out = String::from("pub const COMPILED_KEYS: &[&str] = &[\n");
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        out.push_str(&format!("    include_str!({:?}),\n", f.display().to_string()));
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("compiled_keys.rs");
    std::fs::write(dest, out).unwrap();
}
