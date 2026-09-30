//! Embeds every `migrations/NNNN_<name>.sql` file, so migrations from other streams are picked up
//! without editing this crate.

use std::fmt::Write as _;
use std::path::PathBuf;

#[path = "src/scan.rs"]
#[allow(dead_code)]
mod scan;

fn main() {
    println!("cargo:rerun-if-changed=migrations");
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let dir = PathBuf::from(manifest).join("migrations");
    let found = match scan::scan_dir(&dir) {
        Ok(found) => found,
        Err(e) => panic!("migrations: {e}"),
    };

    let mut out = String::from("&[\n");
    for f in &found {
        // Each file is also a rerun trigger, so editing one rebuilds.
        println!("cargo:rerun-if-changed={}", f.path.display());
        let path = f.path.to_str().expect("migration paths are UTF-8");
        writeln!(
            out,
            "    Migration::embedded({}, {:?}, include_str!({:?})),",
            f.version, f.name, path
        )
        .expect("writing to a String");
    }
    out.push(']');

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    std::fs::write(out_dir.join("migrations.rs"), out).expect("write migrations.rs");
}
