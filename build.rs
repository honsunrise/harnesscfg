//! Embed every `registry/*.toml` into the binary, sorted by id.

use std::fmt::Write;

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("registry");
    println!("cargo::rerun-if-changed={}", dir.display());
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("registry directory")
        .map(|entry| entry.expect("registry entry").path())
        .filter(|path| path.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    let mut out = String::from("pub static FILES: &[(&str, &str)] = &[\n");
    for path in files {
        let stem = path.file_stem().unwrap().to_str().expect("utf-8 file name");
        writeln!(
            out,
            "    ({stem:?}, include_str!({:?})),",
            path.display().to_string()
        )
        .unwrap();
    }
    out.push_str("];\n");
    let target = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("registry.rs");
    std::fs::write(target, out).expect("write generated registry");
}
