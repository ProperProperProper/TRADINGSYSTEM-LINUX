//! Keep only the SPY holdings moomoo can trade: reads a holdings CSV (from ops/spy_holdings_xlsx.py), asks moomoo for the
//! static information of every ticker in paced batches, and writes
//! - `data/spy_holdings.csv`: the holdings that are eligible (NYSE/Nasdaq, not delisting, known to moomoo), the file's own
//!   rank, name and weight kept;
//! - `data/spy_removed.csv`: the holdings left out, with moomoo's reason.
//!
//! The bot's universe is exactly `data/spy_holdings.csv`. Refuses to write when moomoo answered for too few tickers.
//!
//!   cargo run --release --example holdings_check -- <candidate.csv>
use anyhow::{bail, Context, Result};
use moomoo_bot::engine::moomoo::{ineligible, Gateway};

/// Keeps only the SPY holdings moomoo can trade: checks every ticker's static info and writes data/spy_holdings.csv and data/spy_removed.csv.
#[tokio::main]
async fn main() -> Result<()> {
    let src = std::env::args()
        .nth(1)
        .context("usage: holdings_check <candidate.csv>")?;
    let text = std::fs::read_to_string(&src).with_context(|| format!("read {src}"))?;
    let comments: Vec<&str> = text.lines().filter(|l| l.starts_with('#')).collect();
    let rows: Vec<Vec<String>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("rank,") && !l.trim().is_empty())
        .map(|l| l.split(',').map(|c| c.trim().to_string()).collect())
        .filter(|c: &Vec<String>| c.len() >= 4 && !c[1].is_empty())
        .collect();
    if rows.len() < 400 {
        bail!("{src}: only {} holdings, not SPY's full list", rows.len());
    }
    let tickers: Vec<String> = rows.iter().map(|c| c[1].clone()).collect();
    let gw = Gateway::connect("claude-moomoo-holdings-check").await?;
    let info = gw.static_info(&tickers).await?;
    if info.len() < rows.len() * 9 / 10 {
        bail!(
            "moomoo returned static information for only {} of {} holdings: not writing",
            info.len(),
            rows.len()
        );
    }
    let (mut keep, mut removed) = (Vec::new(), Vec::new());
    for c in &rows {
        let why = match info.get(&c[1]) {
            None => Some("not available on moomoo (no static information)".to_string()),
            Some(b) => ineligible(b),
        };
        match why {
            None => keep.push(c.join(",")),
            Some(w) => removed.push(format!("{},{},{}", c[1], c[2], w.replace(',', ";"))),
        }
    }
    let mut out = comments.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    out.push(format!(
        "# Checked on moomoo {}: {} eligible, {} removed (see spy_removed.csv).",
        chrono::Utc::now().format("%Y-%m-%d"),
        keep.len(),
        removed.len()
    ));
    out.push("rank,ticker,name,weight_pct".into());
    out.extend(keep.iter().cloned());
    std::fs::write("data/spy_holdings.csv", out.join("\n") + "\n")?;
    let mut rem = vec!["ticker,name,reason".to_string()];
    rem.extend(removed.iter().cloned());
    std::fs::write("data/spy_removed.csv", rem.join("\n") + "\n")?;
    eprintln!(
        "{} eligible, {} removed: {:?}",
        keep.len(),
        removed.len(),
        removed
    );
    Ok(())
}
