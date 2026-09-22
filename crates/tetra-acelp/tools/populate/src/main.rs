//! Fetches the ETSI EN 300 395-2 reference codec data and generates
//! `src/tables.rs` for the `tetra-acelp` crate.
//!
//! The numeric tables (LSP/gain codebooks, windows, interpolation filters) are
//! not distributed with this crate for copyright reasons. This tool downloads
//! the reference archive directly from ETSI's free deliver server (or reads a
//! local copy) and regenerates the table module locally.
//!
//! Usage:
//!   cargo run -p populate                 # download from ETSI
//!   cargo run -p populate -- <file.zip>   # use a local copy of the archive

use std::io::{Cursor, Read};
use std::path::Path;

const URL: &str =
    "https://www.etsi.org/deliver/etsi_en/300300_300399/30039502/01.03.01_60/en_30039502v010301p0.zip";

fn main() {
    if let Err(e) = run() {
        eprintln!("populate: error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arg = std::env::args().nth(1);
    let bytes = match arg {
        Some(path) => {
            println!("populate: reading local archive {path}");
            std::fs::read(&path).map_err(|e| format!("reading {path}: {e}"))?
        }
        None => {
            println!("populate: downloading ETSI reference archive...");
            download(URL)?
        }
    };
    println!("populate: archive is {} bytes", bytes.len());

    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("opening zip: {e}"))?;

    let clsp = read_entry(&mut zip, "CLSP_334.TAB")?;
    let ener = read_entry(&mut zip, "ENER_QUA.TAB")?;
    let window = read_entry(&mut zip, "WINDOW.TAB")?;
    let lag = read_entry(&mut zip, "LAG_WIND.TAB")?;
    let grid = read_entry(&mut zip, "GRID.TAB")?;
    let log2 = read_entry(&mut zip, "LOG2.TAB")?;
    let sub = read_entry(&mut zip, "SUB_SC_D.C")?;

    let tables: &[(&str, &str, Vec<i16>)] = &[
        (
            "WINDOW",
            "Asymmetric analysis window for LP autocorrelation (256 samples, Q15).",
            array(&window, "window", 256)?,
        ),
        (
            "LAG_H",
            "Lag-window coefficients, high DPF part (eq. 9/10).",
            array(&lag, "lag_h", 10)?,
        ),
        (
            "LAG_L",
            "Lag-window coefficients, low DPF part (eq. 9/10).",
            array(&lag, "lag_l", 10)?,
        ),
        (
            "GRID",
            "Cosine grid used when locating LSP roots (Q15).",
            array(&grid, "grid", 61)?,
        ),
        (
            "DICO1_CLSP",
            "LSP split-VQ sub-codebook 1: 256 x 3.",
            array(&clsp, "dico1_clsp", 768)?,
        ),
        (
            "DICO2_CLSP",
            "LSP split-VQ sub-codebook 2: 512 x 3.",
            array(&clsp, "dico2_clsp", 1536)?,
        ),
        (
            "DICO3_CLSP",
            "LSP split-VQ sub-codebook 3: 512 x 4.",
            array(&clsp, "dico3_clsp", 2048)?,
        ),
        (
            "T_QUA_ENER",
            "Gain prediction-error VQ codebook: 64 x 2 (Q8).",
            array(&ener, "t_qua_ener", 128)?,
        ),
        (
            "TAB_LOG2",
            "Interpolation table for log2 (Q15).",
            array(&log2, "tab_log2", 33)?,
        ),
        (
            "INTER8_M1_3",
            "8-tap fractional interpolation at -1/3.",
            coef(&sub, "Inter8_M1_3", 8)?,
        ),
        (
            "INTER8_1_3",
            "8-tap fractional interpolation at +1/3.",
            coef(&sub, "Inter8_1_3", 8)?,
        ),
        (
            "INTER32_M1_3",
            "32-tap fractional interpolation at -1/3.",
            coef(&sub, "Inter32_M1_3", 32)?,
        ),
        (
            "INTER32_1_3",
            "32-tap fractional interpolation at +1/3.",
            coef(&sub, "Inter32_1_3", 32)?,
        ),
    ];

    let out = generate(tables);
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/tables.rs");
    std::fs::write(&target, out).map_err(|e| format!("writing {}: {e}", target.display()))?;
    println!("populate: wrote {}", target.display());
    for (name, _, vals) in tables {
        println!("  {name}: {} values", vals.len());
    }
    println!("populate: done. Run `cargo test` to verify.");
    Ok(())
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| format!("HTTP request failed: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .map_err(|e| format!("reading response: {e}"))?;
    Ok(buf)
}

/// Read a zip entry by case-insensitive filename suffix.
fn read_entry<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    suffix: &str,
) -> Result<String, String> {
    let suffix_l = suffix.to_ascii_lowercase();
    let idx = (0..zip.len()).find(|&i| {
        zip.by_index(i)
            .map(|f| f.name().to_ascii_lowercase().ends_with(&suffix_l))
            .unwrap_or(false)
    });
    let idx = idx.ok_or_else(|| format!("entry {suffix} not found in archive"))?;
    let mut f = zip.by_index(idx).map_err(|e| e.to_string())?;
    let mut s = String::new();
    f.read_to_string(&mut s).map_err(|e| e.to_string())?;
    Ok(s)
}

/// Remove C comments so array bodies can be parsed cleanly.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if i + 1 < b.len() && b[i] == b'/' && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if i + 1 < b.len() && b[i] == b'/' && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

/// Evaluate one array element token, e.g. "16231 * 2" or "-49", wrapping to i16.
fn eval_token(tok: &str) -> Result<i16, String> {
    let mut acc: i64 = 1;
    let mut any = false;
    for factor in tok.split('*') {
        let t = factor.trim();
        if t.is_empty() {
            continue;
        }
        let v: i64 = t.parse().map_err(|_| format!("bad number {t:?}"))?;
        acc *= v;
        any = true;
    }
    if !any {
        return Err(format!("empty token in {tok:?}"));
    }
    Ok((acc as u16) as i16)
}

/// Parse `<name>[...] = { ... };` from a source string into `expect` values.
fn array(src: &str, name: &str, expect: usize) -> Result<Vec<i16>, String> {
    let s = strip_comments(src);
    let start = s
        .find(name)
        .ok_or_else(|| format!("array {name} not found"))?;
    let brace = s[start..].find('{').ok_or("missing {")? + start;
    let end = s[brace..].find('}').ok_or("missing }")? + brace;
    let body = &s[brace + 1..end];
    let vals: Result<Vec<i16>, String> = body
        .split(',')
        .filter(|t| !t.trim().is_empty())
        .map(eval_token)
        .collect();
    let vals = vals?;
    if vals.len() != expect {
        return Err(format!(
            "{name}: expected {expect} values, got {}",
            vals.len()
        ));
    }
    Ok(vals)
}

/// Parse the `coef[N] = { ... }` array inside a named function.
fn coef(src: &str, func: &str, expect: usize) -> Result<Vec<i16>, String> {
    let s = strip_comments(src);
    let fstart = s
        .find(func)
        .ok_or_else(|| format!("function {func} not found"))?;
    let cpos = s[fstart..]
        .find("coef[")
        .ok_or_else(|| format!("coef[] not found in {func}"))?
        + fstart;
    let brace = s[cpos..].find('{').ok_or("missing {")? + cpos;
    let end = s[brace..].find('}').ok_or("missing }")? + brace;
    let body = &s[brace + 1..end];
    let vals: Result<Vec<i16>, String> = body
        .split(',')
        .filter(|t| !t.trim().is_empty())
        .map(eval_token)
        .collect();
    let vals = vals?;
    if vals.len() != expect {
        return Err(format!(
            "{func}: expected {expect} coeffs, got {}",
            vals.len()
        ));
    }
    Ok(vals)
}

fn generate(tables: &[(&str, &str, Vec<i16>)]) -> String {
    let mut out = String::new();
    out.push_str(
        "//! Numeric data tables from the ETSI TETRA codec (EN 300 395-2).\n\
         //!\n\
         //! GENERATED by `cargo run -p populate` from the ETSI reference archive.\n\
         //! This file is not committed (see .gitignore): the codebooks/windows are\n\
         //! reproduced from the normative ETSI reference, which you obtain from ETSI.\n\n",
    );
    for (name, doc, vals) in tables {
        out.push_str(&format!("/// {doc}\n"));
        out.push_str(&format!("pub const {name}: [i16; {}] = [\n", vals.len()));
        for chunk in vals.chunks(12) {
            out.push_str("    ");
            for v in chunk {
                out.push_str(&format!("{v}, "));
            }
            out.push('\n');
        }
        out.push_str("];\n\n");
    }
    out
}
