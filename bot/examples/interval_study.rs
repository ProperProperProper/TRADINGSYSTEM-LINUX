//! Release check for every candle interval (`Interval::ALL`: 15m, 1h, 2h, 3h, 4h, 5h, one session) on REAL moomoo data: today's
//! universe (SPY's holdings, NYSE/Nasdaq, nothing delisted), rules from real snapshot quotes and the
//! bot's stored real fees (commission excluded when none exist), each interval built exactly like
//! the service does (15m candles aggregated per session, 1h / 4h / one session with warm-up candles
//! before their 21-session window), then one full search per interval. Prints what the service's
//! interval table will show. Never places orders. Respects the 7-day candle quota (a symbol not
//! pulled in the window is skipped once only the reserve is left).
//!
//!   cargo run --release --example interval_study
use anyhow::{Context, Result};
use moomoo_bot::engine::moomoo::{
    build_market, check_calendar, quota_allows, rules_from_quote, Gateway, SymbolData,
    QUOTA_RESERVE,
};
use moomoo_bot::engine::walkforward::{self, SearchSpace};
use moomoo_bot::engine::{scores, xs, Bar, Interval, REFERENCE};
use rusqlite::{Connection, OpenFlags};
use std::time::{Duration, Instant};

/// Real fee observations (notional, fee) from the bot's store, if it exists.
fn stored_fees() -> Vec<(f64, f64)> {
    let Ok(db) = Connection::open_with_flags(
        moomoo_bot::platform::bot_store_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return vec![];
    };
    let Ok(mut st) = db.prepare("SELECT qty * price, total FROM order_fees") else {
        return vec![];
    };
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|x| x.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Release check of every candle interval on real moomoo data: builds each interval like the service and runs one full search per interval.
#[tokio::main]
async fn main() -> Result<()> {
    let gw = Gateway::connect("claude-moomoo-interval-study").await?;
    let sessions = gw.sessions().await?;
    let now = chrono::Utc::now().timestamp_millis();
    let plans: Vec<_> = Interval::ALL
        .into_iter()
        .map(|i| {
            let s = sessions.with_interval(i);
            let w = s.last_closed_bars(now, i.test_candles() + i.warmup_candles());
            (i, s, w)
        })
        .collect();
    let oldest = plans
        .iter()
        .filter_map(|p| p.2.first().copied())
        .min()
        .unwrap();
    let tickers =
        moomoo_bot::engine::moomoo::load_holdings(std::path::Path::new("data/spy_holdings.csv"))
            .context("data/spy_holdings.csv missing")?;
    let (pool, excluded) = gw
        .universe_from(&tickers, moomoo_bot::engine::moomoo::SPY_POOL)
        .await?;
    eprintln!(
        "screener pool: {} symbols ({} rows left out; first: {:?})",
        pool.len(),
        excluded.len(),
        excluded.first()
    );
    let fees = stored_fees();
    let syms: Vec<String> = pool.iter().map(|l| l.symbol.clone()).collect();
    let quotes = gw.quotes(&syms).await?;
    let mut rules = std::collections::HashMap::new();
    for l in &pool {
        match quotes
            .get(&l.symbol)
            .ok_or_else(|| anyhow::anyhow!("no snapshot"))
            .and_then(|q| rules_from_quote(l, q, &fees, now))
        {
            Ok(r) => {
                rules.insert(l.symbol.clone(), r);
            }
            Err(e) => eprintln!("  EXCLUDED {}: no rules ({e:#})", l.symbol),
        }
    }
    if let Some(r) = rules.values().next() {
        eprintln!("commission: {}", r.commission_source);
    }
    // DATA_CACHE=<file>: reuse candles fetched by an earlier run, else fetch.
    let cache = std::env::var("DATA_CACHE").ok();
    type Row = (String, Vec<(i64, Bar)>, i64);
    let cached: Option<Vec<Row>> = cache
        .as_ref()
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice(&b).ok());
    let rows: Vec<Row> = match cached {
        Some(r) => {
            eprintln!("{} symbols from the data cache", r.len());
            r
        }
        None => {
            let mut quota = gw.quota().await?;
            eprintln!(
                "candle quota before: used {}, remaining {}",
                quota.used, quota.remain
            );
            let mut rows = Vec::new();
            for sym in syms
                .iter()
                .filter(|s| rules.contains_key(*s))
                .cloned()
                .chain(std::iter::once(REFERENCE.to_string()))
            {
                let reserve = if sym == REFERENCE { 0 } else { QUOTA_RESERVE };
                if !quota_allows(&quota, &sym, reserve) {
                    eprintln!(
                        "  EXCLUDED {sym}: candle quota (remaining {})",
                        quota.remain
                    );
                    continue;
                }
                match gw.bars(&sym, oldest, &sessions).await {
                    Ok((trades, off)) => {
                        if off > 0 {
                            eprintln!("  {sym}: {off} candles outside the calendar dropped");
                        }
                        let listing_wall = *trades.keys().next().unwrap_or(&0);
                        rows.push((sym.clone(), trades.into_iter().collect(), listing_wall));
                    }
                    Err(e) => eprintln!("  EXCLUDED {sym}: candles unavailable ({e:#})"),
                }
                if !quota.pulled.contains_key(&sym) {
                    quota.pulled.insert(sym.clone(), "now".into());
                    quota.used += 1;
                    quota.remain -= 1;
                }
            }
            let q = gw.quota().await?;
            eprintln!(
                "candle quota after: used {}, remaining {} (moomoo)",
                q.used, q.remain
            );
            if let Some(f) = &cache {
                std::fs::write(f, serde_json::to_vec(&rows)?)?;
            }
            rows
        }
    };
    if let Some(r) = rows.iter().find(|r| r.0 == REFERENCE) {
        let candles = r.1.iter().copied().collect();
        let last = sessions.last_closed_bars(now, 1)[0] + moomoo_bot::engine::BAR_MS;
        let (closed, problems) = check_calendar(&sessions, &candles, oldest, last);
        eprintln!(
            "calendar vs {REFERENCE} candles ({} .. {}): unscheduled closures {:?}, problems {:?}",
            moomoo_bot::engine::moomoo::fmt_et(oldest),
            moomoo_bot::engine::moomoo::fmt_et(last),
            closed,
            problems
        );
    }
    let data: Vec<SymbolData> = rows
        .into_iter()
        .map(|(sym, t, listing_wall)| {
            let r = rules.get(&sym).cloned();
            SymbolData {
                eligible: r.is_some() && sym != REFERENCE,
                symbol: sym,
                rules: r,
                trades: t.into_iter().collect(),
                listing_wall,
            }
        })
        .collect();
    drop(gw);
    let equity = 1000.0;
    let mut common_start = 0;
    let mut frames = Vec::new();
    for (i, s, w) in &plans {
        let raw = build_market(s, w, &data)?;
        let bad: Vec<String> = (0..raw.symbols.len())
            .filter(|&k| raw.validate_symbol(k).is_err())
            .map(|k| raw.symbols[k].clone())
            .collect();
        let keep: Vec<SymbolData> = data
            .iter()
            .filter(|d| !bad.contains(&d.symbol))
            .map(|d| SymbolData {
                symbol: d.symbol.clone(),
                trades: d.trades.clone(),
                listing_wall: d.listing_wall,
                eligible: d.eligible,
                rules: d.rules.clone(),
            })
            .collect();
        let m = build_market(s, w, &keep)?;
        m.validate()?;
        let test_start = w.len() - i.test_candles();
        common_start = common_start.max(m.wall[test_start]);
        eprintln!(
            "{}: {} candles ({} warm-up + {} test, {} sessions), {} symbols, {} dropped for incomplete candles {:?}",
            i.label(), m.ts.len(), test_start, i.test_candles(), i.test_sessions(), m.symbols.len(), bad.len(), bad
        );
        frames.push((*i, m, test_start));
    }
    println!("\ninterval   window   usable/evaluated  winner return  trades  maxDD   last-14-sessions  secs");
    for (i, m, test_start) in &frames {
        let started = Instant::now();
        let sc = scores::compute(m, walkforward::UNIVERSE);
        let deadline = Instant::now() + Duration::from_secs(12 * 60);
        let rep = match i {
            Interval::M15 => walkforward::search_live(m, &sc, equity, deadline, 7, &[])?,
            _ => walkforward::search_random(
                m,
                &sc,
                equity,
                deadline,
                7,
                &[],
                SearchSpace {
                    warmup_bars: *test_start,
                    ..walkforward::space_for(*i)
                },
            )?,
        };
        let warm = sc
            .iter()
            .map(|row| row[*test_start..].iter().filter(|x| x.is_some()).count())
            .max()
            .unwrap_or(0);
        let line = match rep.params.as_ref() {
            Some(p) => {
                let full = xs::backtest(m, &sc, *test_start..m.ts.len(), p, equity);
                let mt = full.metrics();
                // Equity at the start of the shared span, from the same continuous run.
                let mut pf = xs::XsPortfolio::new(equity);
                let mut base = equity;
                for t in *test_start..m.ts.len() {
                    if m.wall[t] <= common_start {
                        base = pf.equity + pf.unrealized();
                    }
                    pf.step(m, &sc, t, p);
                }
                format!(
                    "{:>+12.2}%  {:>6}  {:>5.1}%  {:>+15.2}%",
                    mt.return_pct(),
                    mt.trades,
                    mt.max_drawdown_pct,
                    (full.equity / base - 1.0) * 100.0
                )
            }
            None => "   no settings pass the safety rules".to_string(),
        };
        // Where do sampled settings fail? (2,000 draws of the same space.)
        let band = walkforward::MarginBand::new(&m.slice(*test_start..m.ts.len()))?;
        let space = match i {
            Interval::M15 => walkforward::LIVE_SPACE,
            _ => walkforward::space_for(*i),
        };
        let draws = walkforward::sample_space(11, 2000, &band, space);
        let (mut zero, mut errs, mut ge8, mut dd, mut maxt) = (0, 0, 0, 0, 0usize);
        let mut first_err = None;
        let mut no_filter_trades = Vec::new();
        for p in &draws {
            let r = xs::backtest(m, &sc, *test_start..m.ts.len(), p, equity).metrics();
            if let Some(e) = &r.execution_error {
                errs += 1;
                first_err.get_or_insert(e.clone());
            }
            if r.trades == 0 {
                zero += 1
            }
            if r.trades >= 8 {
                ge8 += 1
            }
            if r.max_drawdown_pct > 25.0 {
                dd += 1
            }
            maxt = maxt.max(r.trades);
            let mut q = p.clone();
            q.regime = xs::Regime::Off;
            no_filter_trades.push(
                xs::backtest(m, &sc, *test_start..m.ts.len(), &q, equity)
                    .metrics()
                    .trades,
            );
        }
        no_filter_trades.sort();
        eprintln!(
            "  {} diagnostics (2000 draws): no trade {zero}, >=8 trades {ge8}, max trades {maxt}, drawdown>25% {dd}, execution errors {errs} {:?}; without the SPY filter median trades {} max {}",
            i.label(), first_err, no_filter_trades[1000], no_filter_trades[1999]
        );
        println!(
            "{:<9} {:>3} ses  {:>7}/{:<7}   {line}  {:>4.0}   (candles with scores in window: {warm})",
            i.label(),
            i.test_sessions(),
            rep.usable,
            rep.evaluated,
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}
