//! Build guard: the ETSI codebook data (`src/tables.rs`) is not distributed
//! with this crate for copyright reasons. If it is missing, fail early with a
//! clear instruction instead of an opaque "unresolved module" error.

use std::path::Path;

fn main() {
    let tables = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tables.rs");
    println!("cargo:rerun-if-changed=src/tables.rs");
    if !tables.exists() {
        eprintln!("\n========================================================================");
        eprintln!(" tetra-acelp: src/tables.rs is missing.");
        eprintln!();
        eprintln!(" The ETSI reference codebooks/windows are not shipped with this crate.");
        eprintln!(" Generate them once (downloads the free ETSI reference archive):");
        eprintln!();
        eprintln!("     cargo run -p populate");
        eprintln!();
        eprintln!(" or, from a local copy of the archive:");
        eprintln!();
        eprintln!("     cargo run -p populate -- path/to/en_30039502v010301p0.zip");
        eprintln!();
        eprintln!(" See README.md for details.");
        eprintln!("========================================================================\n");
        std::process::exit(1);
    }
}
