//! Cross-checks the primary (moomoo) candles against Yahoo and resolves gaps with real data.
//! Nothing is ever invented: a missing or zero-volume bar is replaced only by a real Yahoo bar with
//! volume, otherwise the span is recorded in `data_gaps` so the engine excludes it.
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::{thread::sleep, time::Duration};

use crate::candles::{Bar, Candles};
use crate::symbols::yahoo_symbol;
use crate::yahoo::{self, Client, Fetch, MAX_1M_DAYS};

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Default)]
pub struct Summary {
    pub symbols_checked: usize,
    pub symbols_unavailable: usize,
    pub overlap_bars: usize,
    pub filled_from_yahoo: usize,
    pub gaps_recorded: usize,
    pub incomplete_symbols: usize,
}

fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    Some(sorted[((sorted.len() - 1) as f64 * p).round() as usize])
}

/// Market calendar: every timestamp at which at least half of the symbols have a bar.
pub fn calendar(c: &Candles) -> Vec<i64> {
    crate::merge::active_calendar(
        &c.iter()
            .map(|(s, bars)| (s.clone(), bars.iter().map(|b| b.ts_ms).collect()))
            .collect::<BTreeMap<_, _>>(),
    )
    .into_iter()
    .collect()
}

/// Groups timestamps into runs that are contiguous in the market calendar.
fn runs(cal_pos: &HashMap<i64, usize>, mut ts: Vec<i64>) -> Vec<Vec<i64>> {
    ts.sort_unstable();
    let mut out: Vec<Vec<i64>> = Vec::new();
    for t in ts {
        match out.last_mut() {
            Some(r) if cal_pos[&r[r.len() - 1]] + 1 == cal_pos[&t] => r.push(t),
            _ => out.push(vec![t]),
        }
    }
    out
}

pub fn run(
    candles: &Candles,
    conn: &Connection,
    now_ms: i64,
    only: Option<&[String]>,
) -> Result<Summary> {
    let client = Client::new()?;
    let window_start = now_ms - MAX_1M_DAYS * DAY_MS;
    let cal = calendar(candles);
    let cal_pos: HashMap<i64, usize> = cal.iter().enumerate().map(|(i, t)| (*t, i)).collect();
    let cal_end = cal.last().copied().unwrap_or(0);
    let mut sum = Summary::default();

    for (sym, bars) in candles {
        if only.is_some_and(|o| !o.contains(sym)) {
            continue;
        }
        let Some(first) = bars.first() else { continue };
        let ysym = yahoo_symbol(sym);
        let from = first.ts_ms.max(window_start);
        let fetched = client.bars_1m(sym, from / 1000, cal_end / 1000 + 900)?;
        sleep(Duration::from_millis(300));
        let (returned, yname, yb): (Option<String>, Option<String>, HashMap<i64, Bar>) =
            match &fetched {
                Fetch::Bars {
                    returned,
                    name,
                    bars,
                } if returned.eq_ignore_ascii_case(&ysym) => (
                    Some(returned.clone()),
                    Some(name.clone()),
                    bars.iter().map(|b| (b.ts_ms, *b)).collect(),
                ),
                Fetch::Bars { returned, name, .. } => {
                    (Some(returned.clone()), Some(name.clone()), HashMap::new())
                }
                Fetch::Unavailable(_) => (None, None, HashMap::new()),
            };
        for table in [
            "data_gaps",
            "source_checks",
            "symbol_coverage",
            "candles_supplement",
        ] {
            conn.execute(&format!("DELETE FROM {table} WHERE moomoo_symbol=?"), [sym])?;
        }
        if yb.is_empty() {
            sum.symbols_unavailable += 1;
        }
        sum.symbols_checked += 1;

        // 1. Agreement on bars both sources have real volume for.
        let (mut diffs, mut ratios, mut overlap, mut missing) =
            (Vec::new(), Vec::new(), 0usize, 0usize);
        for b in bars.iter().filter(|b| b.ts_ms >= from && b.volume > 0.0) {
            match yb.get(&b.ts_ms) {
                Some(y) if y.volume > 0.0 => {
                    overlap += 1;
                    diffs.push((b.close - y.close).abs() / y.close * 1e4);
                    ratios.push(b.volume / y.volume);
                }
                _ => missing += 1,
            }
        }
        diffs.sort_by(f64::total_cmp);
        ratios.sort_by(f64::total_cmp);
        sum.overlap_bars += overlap;
        conn.execute(
            "INSERT INTO source_checks(ts_ms,moomoo_symbol,yahoo_symbol,yahoo_returned_symbol,yahoo_name,overlap_bars,missing_in_yahoo,close_diff_bps_median,close_diff_bps_p99,close_diff_bps_max,volume_ratio_median) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            (now_ms, sym, &ysym, &returned, &yname, overlap, missing, percentile(&diffs, 0.5), percentile(&diffs, 0.99), diffs.last().copied(), percentile(&ratios, 0.5)),
        )?;

        // 2. Bars to resolve: calendar timestamps from the symbol's first bar on that are absent
        //    or have no volume.
        let real: HashSet<i64> = bars
            .iter()
            .filter(|b| b.volume > 0.0)
            .map(|b| b.ts_ms)
            .collect();
        let first_pos = cal.partition_point(|t| *t < first.ts_ms);
        let mut to_resolve: Vec<i64> = cal[first_pos..]
            .iter()
            .copied()
            .filter(|t| !real.contains(t))
            .collect();
        // Zero-volume bars the moomoo feed has that are off the calendar still need resolving.
        to_resolve.extend(
            bars.iter()
                .filter(|b| {
                    (b.volume.is_nan() || b.volume <= 0.0) && !cal_pos.contains_key(&b.ts_ms)
                })
                .map(|b| b.ts_ms),
        );

        let mut filled = 0usize;
        let mut unresolved: Vec<i64> = Vec::new();
        let mut confirmed_zero: HashSet<i64> = HashSet::new();
        for t in to_resolve {
            match yb.get(&t) {
                Some(y) if y.volume > 0.0 => {
                    conn.execute(
                        "INSERT OR REPLACE INTO candles_supplement VALUES(?,?,?,?,?,?,?,?,?,?)",
                        (
                            sym,
                            &ysym,
                            y.ts_ms,
                            y.open,
                            y.high,
                            y.low,
                            y.close,
                            y.volume,
                            yahoo::SOURCE,
                            now_ms,
                        ),
                    )?;
                    filled += 1;
                }
                Some(_) => {
                    confirmed_zero.insert(t);
                    unresolved.push(t);
                }
                None => unresolved.push(t),
            }
        }
        sum.filled_from_yahoo += filled;

        // 3. Record what stays unresolved, so the engine excludes it.
        let mut gap_bars = 0usize;
        let on_cal: Vec<i64> = unresolved
            .iter()
            .copied()
            .filter(|t| cal_pos.contains_key(t))
            .collect();
        for run in runs(&cal_pos, on_cal) {
            let (a, z, n) = (run[0], run[run.len() - 1], run.len());
            gap_bars += n;
            let (kind, detail) = if run.iter().all(|t| confirmed_zero.contains(t)) {
                ("no_trade", "Yahoo also shows zero volume for these bars")
            } else if a == first.ts_ms || z < window_start {
                (
                    "placeholder",
                    "no real bar from moomoo; Yahoo 1m reaches back only 58 days",
                )
            } else {
                ("unavailable", "no real bar from moomoo or Yahoo")
            };
            conn.execute(
                "INSERT OR REPLACE INTO data_gaps VALUES(?,?,?,?,?,?,?,?)",
                (sym, &ysym, a, z, kind, n, detail, now_ms),
            )?;
            sum.gaps_recorded += 1;
        }

        // 4. Coverage for this symbol.
        let expected = cal.len() - first_pos;
        let complete = gap_bars == 0;
        if !complete {
            sum.incomplete_symbols += 1;
        }
        conn.execute(
            "INSERT INTO symbol_coverage VALUES(?,?,?,?,?,?,?,?,?,?)",
            (
                sym,
                &ysym,
                first.ts_ms,
                bars.last().map_or(0, |b| b.ts_ms),
                bars.len(),
                filled,
                gap_bars,
                expected,
                complete,
                now_ms,
            ),
        )?;
    }
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts_ms: i64) -> Bar {
        Bar {
            ts_ms,
            open: 10.0,
            high: 11.0,
            low: 9.0,
            close: 10.0,
            volume: 1.0,
        }
    }

    #[test]
    fn calendar_counts_only_symbols_already_listed() {
        let c = Candles::from([
            ("AAA".to_string(), vec![bar(0), bar(60_000)]),
            ("BBB".to_string(), vec![bar(60_000)]),
            ("CCC".to_string(), vec![bar(60_000)]),
        ]);
        assert_eq!(calendar(&c), vec![0, 60_000]);
    }
}
