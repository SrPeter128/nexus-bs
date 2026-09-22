# Vendored Dependency: tetra-acelp

This crate is vendored (full copy, not a git submodule) from:

- Upstream: <https://github.com/misadeks/libtetra-acelp>
- Commit: `44468c22905ea97aff706d60489842d8c07ceeab` (master, 2026-08-14)
- License: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE` ship with the crate)
- Upstream author: misadeks

## Why vendored

The Nexus-BS build must be self-contained on target machines (field deploys
build offline from a single source tree). A path dependency keeps the whole
toolchain reproducible without network access beyond the one-time ETSI data
fetch described below.

## ETSI reference data (not committed)

`src/tables.rs` (LSP/gain codebooks, analysis windows, interpolation filters)
is generated from the free ETSI EN 300 395-2 reference archive and is
git-ignored, per the upstream convention and ETSI terms for the reference
data. Generate it once with:

```sh
cargo run -p populate            # downloads en_30039502v010301p0.zip from etsi.org
# or, from a local copy:
cargo run -p populate -- /path/to/en_30039502v010301p0.zip
```

`tests/tables.rs` verifies the generated tables against a checksum computed
from the ETSI reference `tab_check.c`, and the `*_oracle` tests differentially
verify operators, DSP, encoder, and decoder against the reference codec.

## Modifications by Nexus-BS

- `Cargo.toml`: dropped the standalone `[workspace]` and `[profile.release]`
  sections (invalid inside the Nexus-BS workspace); added SPDX/provenance
  header.
- `.gitignore`: reduced to the `src/tables.rs` entry.
- No source modifications.
