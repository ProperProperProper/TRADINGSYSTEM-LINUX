//! Which symbols to trade. The bot can hold candles for only about 100 symbols at a time (moomoo quota), and the strategy buys the single best-ranked stock, so
//! WHICH symbols are in the pool matters. A pick is made from the training window only (turnover, volatility, or how strongly a symbol reverses), the engine is
//! then run on that pool, and the pick is judged on the unseen block after it (see `walkfwd::run_universe`). Real data only; nothing here sees a later candle.
use moomoo_bot::engine::{Market, REFERENCE};
use std::ops::Range;

use crate::market::Built;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pick {
    /// Every symbol with enough history in the window.
    All,
    /// The `n` symbols with the highest average turnover.
    Turnover(usize),
    /// The `n` symbols with the biggest price swings (standard deviation of candle returns).
    Volatility(usize),
    /// The `n` symbols that reverse most (most negative correlation of a candle's return with the next one).
    Reversal(usize),
    /// The `n` symbols with the best average rank over turnover, volatility and reversal: liquid, swinging and reverting. An unproven prior (see PLAN A3.13).
    Composite(usize),
}

impl Pick {
    pub fn name(self) -> String {
        match self {
            Pick::All => "all".into(),
            Pick::Turnover(n) => format!("turnover:{n}"),
            Pick::Volatility(n) => format!("volatility:{n}"),
            Pick::Reversal(n) => format!("reversal:{n}"),
            Pick::Composite(n) => format!("composite:{n}"),
        }
    }
    /// From "all", "turnover:100", "volatility:50", "reversal:100".
    pub fn parse(s: &str) -> anyhow::Result<Pick> {
        let (kind, n) = s.split_once(':').map_or((s, None), |(k, n)| (k, Some(n)));
        let size = n;
        let n = || -> anyhow::Result<usize> {
            size.ok_or_else(|| anyhow::anyhow!("'{s}' needs a size, like {s}:100"))?
                .parse()
                .map_err(Into::into)
        };
        Ok(match kind {
            "all" if size.is_none() => Pick::All,
            "all" => anyhow::bail!("'all' does not take a size"),
            "turnover" => Pick::Turnover(n()?),
            "volatility" => Pick::Volatility(n()?),
            "reversal" => Pick::Reversal(n()?),
            "composite" => Pick::Composite(n()?),
            other => anyhow::bail!(
                "unknown universe pick '{other}' (all, turnover:N, volatility:N, reversal:N, composite:N)"
            ),
        })
    }
}

/// One symbol's features over a window.
struct Feat {
    s: usize,
    turnover: f64,
    vol: f64,
    reversal: f64,
    composite: f64,
}

/// A symbol needs bars for at least this share of the window to be picked (a symbol that is mostly missing says nothing).
pub const MIN_COVERAGE: f64 = 0.7;

fn stats(m: &Market, s: usize, w: &Range<usize>) -> Option<(f64, f64, f64)> {
    let bars: Vec<(usize, f64, f64)> = w
        .clone()
        .filter_map(|t| m.bars[s][t].map(|b| (t, b.close, b.turnover)))
        .collect();
    if (bars.len() as f64) < MIN_COVERAGE * w.len() as f64 || bars.len() < 8 {
        return None;
    }
    let turnover = bars.iter().map(|x| x.2).sum::<f64>() / bars.len() as f64;
    // returns between consecutive present candles
    let rets: Vec<f64> = bars
        .windows(2)
        .filter(|p| p[1].0 == p[0].0 + 1 && p[0].1 > 0.0)
        .map(|p| p[1].1 / p[0].1 - 1.0)
        .collect();
    if rets.len() < 6 {
        return None;
    }
    let mean = rets.iter().sum::<f64>() / rets.len() as f64;
    let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / rets.len() as f64;
    let sd = var.sqrt();
    // lag-1 autocorrelation
    let (a, b) = (&rets[..rets.len() - 1], &rets[1..]);
    let (ma, mb) = (
        a.iter().sum::<f64>() / a.len() as f64,
        b.iter().sum::<f64>() / b.len() as f64,
    );
    let cov: f64 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let (va, vb): (f64, f64) = (
        a.iter().map(|x| (x - ma).powi(2)).sum(),
        b.iter().map(|y| (y - mb).powi(2)).sum(),
    );
    let ac = if va > 0.0 && vb > 0.0 {
        cov / (va * vb).sqrt()
    } else {
        0.0
    };
    Some((turnover, sd, -ac))
}

/// Symbol indices chosen from the candles in `window` only. The reference symbol is always kept (the trend filter needs it). Symbols without trading rules
/// are never picked (the bot cannot trade them).
pub fn choose(m: &Market, window: Range<usize>, pick: Pick) -> Vec<usize> {
    let reference = m.symbols.iter().position(|s| s == REFERENCE);
    let mut scored: Vec<Feat> = (0..m.symbols.len())
        .filter(|&s| Some(s) != reference && m.rules[s].is_some())
        .filter_map(|s| {
            stats(m, s, &window).map(|(turnover, vol, reversal)| Feat {
                s,
                turnover,
                vol,
                reversal,
                composite: 0.0,
            })
        })
        .collect();
    if let Pick::Composite(_) = pick {
        // average of the three rank positions (0 = best in that feature); lower is better, stored negated so the common descending sort applies
        let rank_of = |f: fn(&Feat) -> f64| -> Vec<f64> {
            let mut order: Vec<usize> = (0..scored.len()).collect();
            order.sort_by(|&a, &b| f(&scored[b]).total_cmp(&f(&scored[a])));
            let mut r = vec![0.0; scored.len()];
            for (pos, &i) in order.iter().enumerate() {
                r[i] = pos as f64;
            }
            r
        };
        let (rt, rv, rr) = (
            rank_of(|x| x.turnover),
            rank_of(|x| x.vol),
            rank_of(|x| x.reversal),
        );
        for (i, f) in scored.iter_mut().enumerate() {
            f.composite = -(rt[i] + rv[i] + rr[i]) / 3.0;
        }
    }
    let (key, n): (fn(&Feat) -> f64, usize) = match pick {
        Pick::All => (|x| x.turnover, usize::MAX),
        Pick::Turnover(n) => (|x| x.turnover, n),
        Pick::Volatility(n) => (|x| x.vol, n),
        Pick::Reversal(n) => (|x| x.reversal, n),
        Pick::Composite(n) => (|x| x.composite, n),
    };
    scored.sort_by(|a, b| key(b).total_cmp(&key(a)).then(a.s.cmp(&b.s)));
    let mut keep: Vec<usize> = scored
        .into_iter()
        .take(n)
        .map(|x| x.s)
        .chain(reference)
        .collect();
    keep.sort_unstable();
    keep
}

/// The symbols `choose` would pick, ordered best first by the pick's own feature (the reference symbol is not included).
pub fn ranked(m: &Market, window: Range<usize>, pick: Pick) -> Vec<usize> {
    let reference = m.symbols.iter().position(|s| s == REFERENCE);
    let mut keep = choose(m, window.clone(), pick);
    keep.retain(|&s| Some(s) != reference);
    // re-order by the same keys: a descending pass over the chosen set
    let mut feats: Vec<Feat> = keep
        .iter()
        .filter_map(|&s| {
            stats(m, s, &window).map(|(turnover, vol, reversal)| Feat {
                s,
                turnover,
                vol,
                reversal,
                composite: 0.0,
            })
        })
        .collect();
    if let Pick::Composite(_) = pick {
        let rank_of = |f: fn(&Feat) -> f64| -> Vec<f64> {
            let mut order: Vec<usize> = (0..feats.len()).collect();
            order.sort_by(|&a, &b| f(&feats[b]).total_cmp(&f(&feats[a])));
            let mut r = vec![0.0; feats.len()];
            for (pos, &i) in order.iter().enumerate() {
                r[i] = pos as f64;
            }
            r
        };
        let (rt, rv, rr) = (
            rank_of(|x| x.turnover),
            rank_of(|x| x.vol),
            rank_of(|x| x.reversal),
        );
        for (i, f) in feats.iter_mut().enumerate() {
            f.composite = -(rt[i] + rv[i] + rr[i]) / 3.0;
        }
    }
    let key: fn(&Feat) -> f64 = match pick {
        Pick::All | Pick::Turnover(_) => |x| x.turnover,
        Pick::Volatility(_) => |x| x.vol,
        Pick::Reversal(_) => |x| x.reversal,
        Pick::Composite(_) => |x| x.composite,
    };
    feats.sort_by(|a, b| key(b).total_cmp(&key(a)).then(a.s.cmp(&b.s)));
    feats.into_iter().map(|f| f.s).collect()
}

/// The market restricted to the symbols in `keep` (indices into the full market, ascending); everything else is shared with `b`.
pub fn subset(b: &Built, keep: &[usize]) -> Built {
    let m = &b.market;
    fn pick<T: Clone>(v: &[T], keep: &[usize]) -> Vec<T> {
        keep.iter().map(|&i| v[i].clone()).collect()
    }
    let market = Market {
        ts: m.ts.clone(),
        wall: m.wall.clone(),
        symbols: pick(&m.symbols, keep),
        listing_times: pick(&m.listing_times, keep),
        entry_eligible: pick(&m.entry_eligible, keep),
        bars: pick(&m.bars, keep),
        marks: pick(&m.marks, keep),
        funding: pick(&m.funding, keep),
        rules: pick(&m.rules, keep),
    };
    Built {
        market,
        sessions: b.sessions.clone(),
        sessions_count: b.sessions_count,
        rejected: b.rejected.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moomoo_bot::engine::rules::Rules;
    use moomoo_bot::engine::{BAR_MS, Bar};

    fn bar(close: f64, turnover: f64) -> Bar {
        Bar {
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            turnover,
        }
    }

    fn rules() -> Option<Rules> {
        serde_json::from_value(serde_json::json!({"qty_step": 1.0, "min_qty": 1.0, "min_notional": 1.0, "max_market_qty": 1e9, "commission": [], "max_leverage": 1.0, "book": [], "book_ts": 0})).ok()
    }

    /// SPY, a calm liquid stock, a wild illiquid one, and one that zig-zags (reverses every candle), on 40 candles.
    fn market() -> Market {
        let n = 40;
        let mk = |f: &dyn Fn(usize) -> f64, turn: f64| -> Vec<Option<Bar>> {
            (0..n).map(|t| Some(bar(f(t), turn))).collect()
        };
        Market {
            ts: (0..n as i64).map(|t| t * BAR_MS).collect(),
            wall: (0..n as i64).map(|t| t * BAR_MS).collect(),
            symbols: vec![
                "SPY".into(),
                "CALM".into(),
                "WILD".into(),
                "ZIGZAG".into(),
                "NORULES".into(),
            ],
            listing_times: vec![None; 5],
            entry_eligible: vec![true; 5],
            bars: vec![
                mk(&|t| 100.0 + t as f64 * 0.1, 5.0),
                mk(&|t| 50.0 + t as f64 * 0.05, 9.0),
                mk(&|t| 20.0 * (1.0 + 0.2 * ((t * 7 % 5) as f64 - 2.0)), 1.0),
                mk(&|t| if t % 2 == 0 { 30.0 } else { 31.0 }, 3.0),
                mk(&|t| 40.0 + t as f64 * 0.01, 99.0),
            ],
            marks: vec![vec![]; 5],
            funding: vec![vec![]; 5],
            rules: vec![rules(), rules(), rules(), rules(), None],
        }
    }

    fn names(m: &Market, keep: &[usize]) -> Vec<String> {
        keep.iter().map(|&i| m.symbols[i].clone()).collect()
    }

    #[test]
    fn picks_are_parsed_and_named() {
        assert_eq!(Pick::parse("all").unwrap(), Pick::All);
        assert_eq!(Pick::parse("volatility:50").unwrap(), Pick::Volatility(50));
        assert_eq!(Pick::Reversal(100).name(), "reversal:100");
        assert!(Pick::parse("turnover").is_err() && Pick::parse("beta:5").is_err());
        assert!(Pick::parse("all:5").is_err(), "all has no size parameter");
    }

    #[test]
    fn each_pick_chooses_by_its_own_feature_keeps_the_reference_and_skips_untradeable_symbols() {
        let m = market();
        assert!(m.rules[1].is_some(), "the test rules parse");
        assert_eq!(
            names(&m, &choose(&m, 0..40, Pick::Turnover(1))),
            ["SPY", "CALM"],
            "most liquid (the 99-turnover symbol has no rules, so it cannot be traded)"
        );
        assert_eq!(
            names(&m, &choose(&m, 0..40, Pick::Volatility(1))),
            ["SPY", "WILD"],
            "biggest swings"
        );
        assert_eq!(
            names(&m, &choose(&m, 0..40, Pick::Reversal(1))),
            ["SPY", "ZIGZAG"],
            "reverses every candle"
        );
        assert_eq!(
            names(&m, &choose(&m, 0..40, Pick::All)),
            ["SPY", "CALM", "WILD", "ZIGZAG"]
        );
    }

    #[test]
    fn a_pick_uses_only_the_window_it_is_given_and_ignores_symbols_that_are_mostly_missing() {
        let mut m = market();
        // CALM has no bars in the first half: from a window covering only that half it cannot be picked; WILD's later swings do not exist yet
        for t in 0..20 {
            m.bars[1][t] = None;
        }
        assert!(!choose(&m, 0..20, Pick::All).contains(&1));
        assert!(choose(&m, 20..40, Pick::All).contains(&1));
        let before = choose(&m, 0..20, Pick::Volatility(1));
        for row in m.bars.iter_mut() {
            for bar in row.iter_mut().skip(20).flatten() {
                bar.close *= 3.0; // the future changes
            }
        }
        assert_eq!(
            choose(&m, 0..20, Pick::Volatility(1)),
            before,
            "a pick from an earlier window cannot see later candles"
        );
    }
}
