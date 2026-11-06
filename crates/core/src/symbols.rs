//! Symbol translation list: how moomoo and Yahoo spell each symbol, whether the two verifiably refer to
//! the same security, and whether the live bot may trade it. Nothing is assumed: tradability comes from
//! the bot's moomoo-verified universe, and the Yahoo match from measured price agreement or, where moomoo
//! has no candles, Yahoo's returned symbol plus a company-name match.
use anyhow::{Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub fn yahoo_symbol(ticker: &str) -> String {
    ticker.replace('.', "-")
}

/// The market the bot sends with every security code: Qot_Common.QotMarket US = 11
/// (`QOT_US` in the bot's src/engine/moomoo.rs). The security code itself is the plain ticker.
pub const MOOMOO_MARKET: &str = "US(QotMarket=11)";

fn set(conn: &Connection, sql: &str) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// SPY holdings tickers from the bot's holdings file (comment lines and header skipped).
pub fn read_holdings(csv: &Path) -> Result<BTreeSet<String>> {
    let text = fs::read_to_string(csv)?;
    Ok(text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("rank,"))
        .filter_map(|l| l.split(',').nth(1))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect())
}

/// Prices agree closely enough that both sources are clearly the same security.
pub fn prices_match(overlap: i64, median_bps: f64, p99_bps: f64, vol_ratio: f64) -> bool {
    overlap >= 200 && median_bps <= 2.0 && p99_bps <= 50.0 && (0.8..=1.25).contains(&vol_ratio)
}

const STOP: [&str; 16] = [
    "the",
    "inc",
    "corp",
    "corporation",
    "co",
    "company",
    "group",
    "holdings",
    "plc",
    "ltd",
    "llc",
    "class",
    "and",
    "of",
    "us",
    "in",
];

fn fold(c: char) -> char {
    match c {
        'à'..='å' => 'a',
        'ç' => 'c',
        'è'..='ë' => 'e',
        'ì'..='ï' => 'i',
        'ñ' => 'n',
        'ò'..='ö' => 'o',
        'ù'..='ü' => 'u',
        _ => c,
    }
}

/// Significant lowercase words of a company name: accents folded, apostrophes and dots removed.
fn name_words(s: &str) -> Vec<String> {
    let cleaned: String = s
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '\'' | '.' | '\u{2019}'))
        .map(fold)
        .collect();
    cleaned
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2 && !STOP.contains(w))
        .map(str::to_string)
        .collect()
}

/// Company names agree when they share at least one significant word.
pub fn names_match(moomoo: &str, yahoo: &str) -> bool {
    let y = name_words(yahoo);
    name_words(moomoo).iter().any(|w| y.contains(w))
}

struct Check {
    returned: Option<String>,
    name: Option<String>,
    overlap: i64,
    median: Option<f64>,
    p99: Option<f64>,
    ratio: Option<f64>,
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    fields.push(current);
    fields
}

/// Seeds the `symbols` table from the canonical `data/symbol_map.csv` and `holdings_csv` when
/// the retired Moomoo importer snapshot (`data/candles.sqlite3`) is absent on a fresh clone.
pub fn seed_from_canonical_map(
    ours: &Connection,
    map_csv: &Path,
    holdings_csv: &Path,
    now_ms: i64,
) -> Result<BTreeMap<String, usize>> {
    let spy = if holdings_csv.exists() {
        read_holdings(holdings_csv)?
    } else {
        BTreeSet::new()
    };
    let text = fs::read_to_string(map_csv)?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let tx = ours.unchecked_transaction()?;
    tx.execute("DELETE FROM symbols", [])?;
    for line in text.lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let f = parse_csv_line(line);
        if f.len() < 16 {
            continue;
        }
        let moomoo_symbol = &f[0];
        let moomoo_market = &f[1];
        let yahoo_sym = &f[2];
        let name = &f[3];
        let bot_status = &f[4];
        let yahoo_match = &f[5];
        let yahoo_returned = (!f[6].is_empty()).then(|| f[6].clone());
        let yahoo_name = (!f[7].is_empty()).then(|| f[7].clone());
        let overlap_bars: Option<i64> = f[8].parse().ok();
        let close_diff_median: Option<f64> = f[9].parse().ok();
        let close_diff_p99: Option<f64> = f[10].parse().ok();
        let vol_ratio_median: Option<f64> = f[11].parse().ok();
        let moomoo_bars: i64 = f[12].parse().unwrap_or(0);
        let in_spy = i32::from(spy.contains(moomoo_symbol));
        let in_uni = i32::from(bot_status == "tradable");
        let has_c = i32::from(moomoo_bars > 0);
        let evidence = if in_uni == 1 {
            "listed in the canonical symbol map (moomoo-verified universe)"
        } else if has_c == 1 {
            "canonical symbol map entry with historical moomoo bars"
        } else {
            "canonical symbol map entry"
        };
        *counts
            .entry(format!("{bot_status}/{yahoo_match}"))
            .or_default() += 1;
        tx.execute(
            "INSERT INTO symbols VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,0)",
            (
                moomoo_symbol,
                moomoo_market,
                yahoo_sym,
                name,
                bot_status,
                in_spy,
                in_uni,
                has_c,
                yahoo_returned,
                yahoo_name,
                yahoo_match,
                overlap_bars,
                close_diff_median,
                close_diff_p99,
                vol_ratio_median,
                evidence,
            ),
        )?;
    }
    tx.execute("UPDATE symbols SET verified_ms=?", [now_ms])?;
    tx.commit()?;
    Ok(counts)
}

/// Rebuilds the `symbols` table from the bot snapshot (read-only), the holdings file, and the
/// Yahoo checks already stored by `reconcile` / `fetch-extra`. Falls back to the canonical
/// `data/symbol_map.csv` when `bot_snapshot` is absent on a fresh installation.
pub fn build(
    ours: &Connection,
    bot_snapshot: &Path,
    holdings_csv: &Path,
    now_ms: i64,
) -> Result<BTreeMap<String, usize>> {
    if !bot_snapshot.exists() {
        let canonical = Path::new("data/symbol_map.csv");
        if canonical.exists() {
            return seed_from_canonical_map(ours, canonical, holdings_csv, now_ms);
        }
    }
    let bot = Connection::open_with_flags(bot_snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let spy = read_holdings(holdings_csv)?;
    let universe = set(
        &bot,
        "SELECT DISTINCT symbol FROM universe WHERE ts_ms=(SELECT MAX(ts_ms) FROM universe)",
    )?;
    let candles = set(&bot, "SELECT DISTINCT symbol FROM candles")?;
    if universe.is_empty() {
        bail!("bot universe snapshot is empty; refusing to mark anything tradable");
    }
    let names: BTreeMap<String, String> = {
        let mut st = bot.prepare(
            "SELECT symbol, name FROM universe WHERE ts_ms=(SELECT MAX(ts_ms) FROM universe)",
        )?;
        st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?
        .collect::<std::result::Result<_, _>>()?
    };
    let checks: BTreeMap<String, Check> = {
        let mut st = ours.prepare(
            "SELECT moomoo_symbol, yahoo_returned_symbol, yahoo_name, overlap_bars, close_diff_bps_median, close_diff_bps_p99, volume_ratio_median FROM source_checks")?;
        st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Check {
                    returned: r.get(1)?,
                    name: r.get(2)?,
                    overlap: r.get(3)?,
                    median: r.get(4)?,
                    p99: r.get(5)?,
                    ratio: r.get(6)?,
                },
            ))
        })?
        .collect::<std::result::Result<_, _>>()?
    };

    let all: BTreeSet<&String> = spy.iter().chain(&universe).chain(&candles).collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let tx = ours.unchecked_transaction()?;
    tx.execute("DELETE FROM symbols", [])?;
    for t in all {
        let (in_spy, in_uni, has_c) = (spy.contains(t), universe.contains(t), candles.contains(t));
        let (status, evidence) = if in_uni {
            (
                "tradable",
                "listed in the bot's moomoo-verified universe (latest snapshot)",
            )
        } else if has_c {
            (
                "unverified",
                "moomoo supplies candles but the bot universe does not list it; not promotable until confirmed",
            )
        } else {
            (
                "research_only",
                "not in the bot universe and no moomoo candles",
            )
        };
        let ysym = yahoo_symbol(t);
        let name = names.get(t).cloned().unwrap_or_default();
        let c = checks.get(t);
        let returned_ok = c
            .and_then(|c| c.returned.as_deref())
            .is_some_and(|r| r.eq_ignore_ascii_case(&ysym));
        let m = match c {
            None => "unverified",
            Some(c) if !returned_ok && c.returned.is_some() => "mismatch",
            Some(_) if !returned_ok => "unverified",
            Some(c) if c.overlap > 0 => match (c.median, c.p99, c.ratio) {
                (Some(a), Some(b), Some(r)) if prices_match(c.overlap, a, b, r) => "verified",
                _ => "mismatch",
            },
            // No moomoo candles to compare prices with: Yahoo's symbol and company name must agree.
            Some(c) if c.name.as_deref().is_some_and(|n| names_match(&name, n)) => "verified_name",
            Some(_) => "unverified",
        };
        *counts.entry(format!("{status}/{m}")).or_default() += 1;
        tx.execute(
            "INSERT INTO symbols VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,0)",
            (
                t,
                MOOMOO_MARKET,
                &ysym,
                &name,
                status,
                in_spy,
                in_uni,
                has_c,
                c.and_then(|c| c.returned.clone()),
                c.and_then(|c| c.name.clone()),
                m,
                c.map(|c| c.overlap),
                c.and_then(|c| c.median),
                c.and_then(|c| c.p99),
                c.and_then(|c| c.ratio),
                evidence,
            ),
        )?;
    }
    tx.execute("UPDATE symbols SET verified_ms=?", [now_ms])?;
    tx.commit()?;
    Ok(counts)
}

/// Gate for anything that would reach the bot: every symbol must be `tradable` AND its Yahoo mapping
/// must be verified. Returns the offending symbols with the reason.
pub fn check_promotable(conn: &Connection, wanted: &[String]) -> Result<Vec<(String, String)>> {
    let mut bad = Vec::new();
    for t in wanted {
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT bot_status, yahoo_match FROM symbols WHERE moomoo_symbol=?",
                [t],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((s, m)) if s == "tradable" && (m == "verified" || m == "verified_name") => {}
            Some((s, m)) if s == "tradable" => bad.push((t.clone(), format!("yahoo_match {m}"))),
            Some((s, _)) => bad.push((t.clone(), s)),
            None => bad.push((t.clone(), "research_only".to_string())),
        }
    }
    Ok(bad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    #[test]
    fn spellings() {
        assert_eq!(yahoo_symbol("BRK.B"), "BRK-B");
        assert_eq!(yahoo_symbol("AAPL"), "AAPL");
        assert_eq!(MOOMOO_MARKET, "US(QotMarket=11)");
    }

    #[test]
    fn match_rules() {
        assert!(prices_match(900, 0.0, 3.0, 1.0));
        assert!(!prices_match(900, 5.0, 3.0, 1.0), "median too far");
        assert!(!prices_match(50, 0.0, 3.0, 1.0), "too few bars to trust");
        assert!(!prices_match(900, 0.0, 80.0, 1.0), "p99 too far");
        assert!(!prices_match(900, 0.0, 3.0, 3.0), "volume disagrees");
        assert!(names_match("Apple", "Apple Inc."));
        assert!(names_match("NVIDIA", "NVIDIA Corporation"));
        assert!(!names_match("Apple", "Microsoft Corporation"));
        assert!(!names_match("", "Microsoft"));
        assert!(names_match(
            "Caseys General Stores",
            "Casey's General Stores, Inc."
        ));
        assert!(names_match(
            "Estee Lauder",
            "The Estée Lauder Companies Inc."
        ));
        assert!(names_match(
            "JB Hunt Transport Services",
            "J.B. Hunt Transport Services, Inc."
        ));
        assert!(names_match("WR Berkley", "W. R. Berkley Corporation"));
        assert!(names_match("3M", "3M Company"));
        assert!(names_match("AT&T", "AT&T Inc."));
        assert!(
            !names_match("Apple Inc", "Microsoft Inc"),
            "legal suffix alone is not a match"
        );
        assert!(
            !names_match("Vivmark Residential", "AvalonBay Communities, Inc."),
            "real conflict stays flagged"
        );
    }

    #[test]
    fn promotion_gate() {
        let c = Connection::open_in_memory().unwrap();
        store::init(&c).unwrap();
        for (t, st, m) in [
            ("AAPL", "tradable", "verified"),
            ("TSM", "unverified", "verified"),
            ("BAD", "tradable", "mismatch"),
            ("NEW", "tradable", "unverified"),
        ] {
            c.execute("INSERT INTO symbols VALUES(?,?,?,'n',?,0,0,0,NULL,NULL,?,NULL,NULL,NULL,NULL,'t',0)", (t, MOOMOO_MARKET, t, st, m)).unwrap();
        }
        let bad = check_promotable(
            &c,
            &[
                "AAPL".into(),
                "TSM".into(),
                "BAD".into(),
                "NEW".into(),
                "ZZZZ".into(),
            ],
        )
        .unwrap();
        assert_eq!(bad.len(), 4);
        assert!(bad.contains(&("NEW".to_string(), "yahoo_match unverified".to_string())));
        assert!(bad.contains(&("TSM".to_string(), "unverified".to_string())));
        assert!(bad.contains(&("BAD".to_string(), "yahoo_match mismatch".to_string())));
        assert!(bad.contains(&("ZZZZ".to_string(), "research_only".to_string())));
        assert!(check_promotable(&c, &["AAPL".into()]).unwrap().is_empty());
    }
}
