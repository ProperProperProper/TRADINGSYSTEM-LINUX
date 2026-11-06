//! Small shared helpers: hashing, time, canonical JSON.

use anyhow::Result;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// SHA-256 of bytes as hex.
pub fn sha256_bytes(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// SHA-256 of a string as hex.
pub fn sha256_str(s: &str) -> String {
    sha256_bytes(s.as_bytes())
}

/// SHA-256 of a file as hex.
pub fn sha256_file(p: &Path) -> Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Current unix time in seconds.
pub fn now_ts() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// serde_json maps are ordered (BTreeMap) without the `preserve_order` feature, so this is canonical.
pub fn canonical(v: &serde_json::Value) -> String {
    serde_json::to_string(v).expect("json serialises")
}

pub const CODE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Python's `f"{x:g}"`: 6 significant digits, trailing zeros trimmed, exponent form below 1e-4 or from 1e6 (`1.23457e+06`, `1e-05`).
pub fn fmt_g(x: f64) -> String {
    if x == 0.0 { return "0".into(); }
    if !x.is_finite() { return if x.is_nan() { "nan".into() } else if x > 0.0 { "inf".into() } else { "-inf".into() }; }
    let sci = format!("{:.5e}", x);                      // rounded to 6 significant digits; the exponent is that of the ROUNDED value
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let e: i32 = exp.parse().unwrap_or(0);
    let trim = |s: &str| -> String { if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s.to_string() } };
    if !(-4..6).contains(&e) {
        format!("{}e{}{:02}", trim(mant), if e < 0 { '-' } else { '+' }, e.abs())
    } else {
        trim(&format!("{:.*}", (5 - e).max(0) as usize, x))
    }
}

#[cfg(test)]
mod fmt_g_tests {
    use super::fmt_g;
    #[test]
    fn matches_python_g_format() {
        for (x, want) in [(0.0, "0"), (0.5, "0.5"), (12.30, "12.3"), (100.0, "100"), (2.14, "2.14"), (1234567.0, "1.23457e+06"), (100000.0, "100000"),
                          (999999.5, "1e+06"), (0.0001, "0.0001"), (0.00001, "1e-05"), (-1.5, "-1.5"), (49.0, "49"), (1.0 / 3.0, "0.333333")] {
            assert_eq!(fmt_g(x), want, "{x}");
        }
    }
}

/// `YYYY-MM-DD HH:MM UTC` for a Unix timestamp (civil-from-days, no date library).
pub fn utc_minute(ts: i64) -> String {
    let (days, rem) = (ts.div_euclid(86_400), ts.rem_euclid(86_400));
    let z = days + 719_468; let era = z.div_euclid(146_097); let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153; let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem % 3600 / 60)
}

#[cfg(test)]
mod utc_tests {
    #[test]
    fn formats_unix_time() {
        assert_eq!(super::utc_minute(0), "1970-01-01 00:00 UTC");
        assert_eq!(super::utc_minute(1_791_850_464), "2026-10-13 00:14 UTC");
        assert_eq!(super::utc_minute(951_782_400), "2000-02-29 00:00 UTC");
    }
}
