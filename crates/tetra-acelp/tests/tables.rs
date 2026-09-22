//! Verifies the generated data tables are bit-identical to the ETSI reference.
//!
//! Folds every table element into the same FNV-style checksum computed by the
//! reference table oracle (`tab_check.c`). A match proves the codebooks and
//! windows in `src/tables.rs` reproduce the reference data exactly.

use tetra_acelp::tables::*;

const FNV_PRIME: u64 = 1099511628211;
const FNV_OFFSET: u64 = 1469598103934665603;
const EXPECTED_CHECKSUM: u64 = 1058747861142533732;

fn fold(mut ck: u64, a: &[i16]) -> u64 {
    for &v in a {
        ck = ck.wrapping_mul(FNV_PRIME).wrapping_add(v as u16 as u64);
    }
    ck
}

#[test]
fn tables_match_reference() {
    let mut ck = FNV_OFFSET;
    ck = fold(ck, &WINDOW);
    ck = fold(ck, &LAG_H);
    ck = fold(ck, &LAG_L);
    ck = fold(ck, &GRID);
    ck = fold(ck, &DICO1_CLSP);
    ck = fold(ck, &DICO2_CLSP);
    ck = fold(ck, &DICO3_CLSP);
    ck = fold(ck, &T_QUA_ENER);
    assert_eq!(ck, EXPECTED_CHECKSUM, "data tables diverged from reference");
}
