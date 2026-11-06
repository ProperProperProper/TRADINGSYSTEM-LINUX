//! Trading intelligence beyond the ranking (owner 2026-10-13: "I want it to be super trading intelligence"): the pieces a trader needs around a stock ranking.
//!   * a RISK model: forecasts SPY's volatility over the next session (a HAR-style ridge on log realised volatilities) and says whether it beats the naive "tomorrow
//!     looks like today" forecast, on rolling out-of-sample windows;
//!   * a DIRECTION model: probability that SPY rises over the next session, with an honest out-of-sample AUC interval (it is expected to show NO skill; the report says so);
//!   * a DECISION engine (`decide`): regime, stance, which names are worth trading AFTER the bot's measured costs, position sizes scaled by risk, and a confidence level taken
//!     from the model's own live record. It abstains when the edge is below costs or the model's recent record is significantly bad.
//!
//! Advisory only: nothing here places an order or touches the live strategy.

use crate::aimodel::{Pick, Scoreboard};
use crate::candles::{Bar, Costs};
use crate::mlcandle::{et_offset_ms, Ridge, Sample};
use std::collections::BTreeMap;

const MS15: i64 = 900_000;
const DAY_MS: i64 = 86_400_000;
const H: usize = 26;

/// One hourly observation of the market state and its outcomes.
#[derive(Debug, Clone)]
pub struct Obs { pub ts: i64, pub week: i64, pub x: Vec<f64>, pub log_rv: f64, pub up: bool, pub next_ret: f64 }

pub const NAMES: [&str; 9] = ["log_rv_26", "log_rv_130", "abs_ret_26", "ret_26", "ret_130", "dist_ma_130", "range_26", "bar_in_day", "log_rv_4"];

fn rv(r: &[f64]) -> f64 { (r.iter().map(|x| x * x).sum::<f64>()).sqrt().max(1e-6) } // realised volatility over the window (root of summed squared 15-minute returns)

/// SPY's state at every hourly bar with the outcomes of the next `H` bars (training / evaluation).
pub fn observations(spy: &[Bar], live: bool) -> Vec<Obs> {
    let r: Vec<f64> = std::iter::once(0.0).chain(spy.windows(2).map(|w| (w[1].close / w[0].close).ln())).collect();
    let (mut bid, mut day, mut n) = (vec![0.0; spy.len()], i64::MIN, 0.0);
    for (i, b) in spy.iter().enumerate() { let d = (b.ts + et_offset_ms(b.ts)).div_euclid(DAY_MS); if d != day { day = d; n = 0.0; } bid[i] = n; n += 1.0; }
    let mut out = vec![];
    let end = if live { spy.len() } else { spy.len().saturating_sub(H) };
    for i in 135..end {
        if (spy[i].ts / MS15) % 4 != 0 && !live { continue; }
        let labelled = i + H < spy.len();
        if live && i + 4 < spy.len() { continue; }
        let c = spy[i].close;
        let (r26, r130, r4) = (&r[i - 25..=i], &r[i - 129..=i], &r[i - 3..=i]);
        let ma130 = spy[i - 129..=i].iter().map(|b| b.close).sum::<f64>() / 130.0;
        let (hi, lo) = spy[i - 25..=i].iter().fold((f64::MIN, f64::MAX), |a, b| (a.0.max(b.high), a.1.min(b.low)));
        let (log_rv, up, next_ret) = if labelled { (rv(&r[i + 1..=i + H]).ln(), spy[i + H].close > c, (spy[i + H].close / c).ln()) } else { (f64::NAN, false, f64::NAN) };
        out.push(Obs { ts: spy[i].ts, week: spy[i].ts.div_euclid(7 * DAY_MS), x: vec![rv(r26).ln(), rv(r130).ln(), (c / spy[i - 26].close).ln().abs(), (c / spy[i - 26].close).ln(), (c / spy[i - 130].close).ln(), c / ma130 - 1.0, (hi - lo) / c, bid[i], rv(r4).ln()], log_rv, up, next_ret });
    }
    out
}

#[derive(Debug, Clone)]
pub struct RiskReport { pub corr_model: f64, pub corr_naive: f64, pub mse_model: f64, pub mse_naive: f64, pub gain_lo: f64, pub gain_hi: f64, pub windows: usize }

/// The rolling-origin forecasts behind [`test_volatility`]: (model, naive, realised, week, time) per test observation, all in log volatility; None when too few.
fn rolling_vol(obs: &[Obs]) -> Option<(Vec<f64>, Vec<f64>, Vec<f64>, Vec<i64>, Vec<i64>)> {
    let (mut preds, mut naive, mut truth, mut weeks, mut times) = (vec![], vec![], vec![], vec![], vec![]);
    let t0 = obs.first()?.ts; let t1 = obs.last()?.ts;
    let mut start = t0 + 60 * DAY_MS;
    while start < t1 {
        let end = start + 28 * DAY_MS;
        let train: Vec<&Obs> = obs.iter().filter(|o| o.ts <= start - 2 * DAY_MS && o.log_rv.is_finite()).collect();
        let test: Vec<&Obs> = obs.iter().filter(|o| o.ts >= start && o.ts < end && o.log_rv.is_finite()).collect();
        if train.len() > 300 && test.len() > 50 {
            let s: Vec<Sample> = train.iter().map(|o| Sample { ts: o.ts, week: o.week, sym: 0, x: o.x.clone(), excess: o.log_rv, raw: 0.0 }).collect();
            let r = Ridge::fit(&s, 1.0);
            let bias = train.iter().map(|o| o.log_rv - o.x[0]).sum::<f64>() / train.len() as f64; // naive: today's volatility plus the average drift between the two
            for o in &test {
                let sm = Sample { ts: o.ts, week: o.week, sym: 0, x: o.x.clone(), excess: 0.0, raw: 0.0 };
                preds.push(r.predict(&sm)); naive.push(o.x[0] + bias); truth.push(o.log_rv); weeks.push(o.week); times.push(o.ts);
            }
        }
        start = end;
    }
    if truth.len() < 200 { return None; }
    Some((preds, naive, truth, weeks, times))
}

/// Rolling-origin test of the volatility forecast against the naive forecast (next volatility = today's, scaled by the training mean ratio). Gain = reduction in squared
/// error, interval by resampling whole weeks.
pub fn test_volatility(obs: &[Obs]) -> Option<RiskReport> {
    let (preds, naive, truth, weeks, _) = rolling_vol(obs)?;
    let mse = |p: &[f64]| p.iter().zip(&truth).map(|(a, b)| (a - b).powi(2)).sum::<f64>() / truth.len() as f64;
    let corr = |p: &[f64]| { let n = p.len() as f64; let (mp, mt) = (p.iter().sum::<f64>() / n, truth.iter().sum::<f64>() / n); let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0); for (a, b) in p.iter().zip(&truth) { sxy += (a - mp) * (b - mt); sxx += (a - mp).powi(2); syy += (b - mt).powi(2); } sxy / (sxx * syy).sqrt().max(1e-12) };
    let mut by: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
    for i in 0..truth.len() { let e = by.entry(weeks[i]).or_default(); e.0 += (naive[i] - truth[i]).powi(2) - (preds[i] - truth[i]).powi(2); e.1 += 1; }
    let stat: Vec<(f64, usize)> = by.values().copied().collect();
    let (_, lo, hi) = crate::mlimprove::interval(&stat, 0.025, 5);
    Some(RiskReport { corr_model: corr(&preds), corr_naive: corr(&naive), mse_model: mse(&preds), mse_naive: mse(&naive), gain_lo: lo, gain_hi: hi, windows: stat.len() })
}

/// SPY volatility (percent over the next session): the model's forecast, the naive forecast and what was realised, averaged per day, out of sample (the chart on the dashboard).
pub fn volatility_track(obs: &[Obs]) -> Vec<serde_json::Value> {
    let Some((p, n, t, _, ts)) = rolling_vol(obs) else { return vec![] };
    let mut by: BTreeMap<i64, (f64, f64, f64, f64)> = BTreeMap::new();
    for i in 0..t.len() { let e = by.entry(ts[i].div_euclid(DAY_MS)).or_default(); e.0 += p[i].exp() * 100.0; e.1 += n[i].exp() * 100.0; e.2 += t[i].exp() * 100.0; e.3 += 1.0; }
    by.into_iter().map(|(d, e)| serde_json::json!({"t": d * 86_400, "model": e.0 / e.3, "naive": e.1 / e.3, "realised": e.2 / e.3})).collect()
}

#[derive(Debug, Clone)]
pub struct DirectionReport { pub auc: f64, pub lo: f64, pub hi: f64, pub n: usize }

/// Rolling-origin test of "will SPY be higher after the next session": AUC with a week-resampled interval. No skill = an interval around 0.5.
pub fn test_direction(obs: &[Obs]) -> Option<DirectionReport> {
    use crate::mltrade::{auc, Logistic, Trade};
    let (mut score, mut label, mut weeks) = (vec![], vec![], vec![]);
    let t0 = obs.first()?.ts; let t1 = obs.last()?.ts;
    let mut start = t0 + 60 * DAY_MS;
    while start < t1 {
        let end = start + 28 * DAY_MS;
        let tr: Vec<Trade> = obs.iter().filter(|o| o.ts <= start - 2 * DAY_MS && o.next_ret.is_finite()).map(|o| Trade { block: o.week, x: o.x.clone(), win: o.up, ret: o.next_ret }).collect();
        let te: Vec<&Obs> = obs.iter().filter(|o| o.ts >= start && o.ts < end && o.next_ret.is_finite()).collect();
        if tr.len() > 300 && te.len() > 50 {
            let m = Logistic::fit(&tr, 120, 0.3, 1e-3);
            for o in te { score.push(m.predict(&Trade { block: o.week, x: o.x.clone(), win: o.up, ret: o.next_ret })); label.push(o.up); weeks.push(o.week); }
        }
        start = end;
    }
    if score.len() < 200 { return None; }
    let a = auc(&score, &label);
    let mut ws: Vec<i64> = weeks.clone(); ws.sort(); ws.dedup();
    let mut st = 99u64; let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; st };
    let mut draws: Vec<f64> = (0..400).map(|_| {
        let picked: Vec<i64> = (0..ws.len()).map(|_| ws[(rnd() % ws.len() as u64) as usize]).collect();
        let idx: Vec<usize> = (0..score.len()).filter(|&i| picked.contains(&weeks[i])).collect();
        if idx.len() < 50 { 0.5 } else { auc(&idx.iter().map(|&i| score[i]).collect::<Vec<_>>(), &idx.iter().map(|&i| label[i]).collect::<Vec<_>>()) }
    }).collect();
    draws.sort_by(f64::total_cmp);
    Some(DirectionReport { auc: a, lo: draws[10], hi: draws[389], n: score.len() })
}

/// A volatility forecast for the newest bar (log realised volatility over the next session, as a percentile of the forecasts made over the history).
pub fn forecast_now(obs_labelled: &[Obs], live: &Obs) -> Option<(f64, f64)> {
    let train: Vec<Sample> = obs_labelled.iter().filter(|o| o.log_rv.is_finite()).map(|o| Sample { ts: o.ts, week: o.week, sym: 0, x: o.x.clone(), excess: o.log_rv, raw: 0.0 }).collect();
    if train.len() < 300 { return None; }
    let r = Ridge::fit(&train, 1.0);
    let probe = |o: &Obs| r.predict(&Sample { ts: o.ts, week: o.week, sym: 0, x: o.x.clone(), excess: 0.0, raw: 0.0 });
    let hist: Vec<f64> = obs_labelled.iter().rev().take(1500).map(&probe).collect();
    let now = probe(live);
    Some((now.exp(), hist.iter().filter(|h| **h < now).count() as f64 / hist.len() as f64))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stance { RiskOn, Reduced, StandAside }

/// The model's call for ONE stock if its signal fires: take or skip, with the numbers behind it.
#[derive(Debug, Clone)]
pub struct Take { pub symbol: String, pub take: bool, pub pct: f64, pub expected_bps: f64, pub net_bps: f64, pub size_usd: f64 }

#[derive(Debug, Clone)]
pub struct Assessment { pub stance: Stance, pub trust: String, pub calls: Vec<Take>, pub notes: Vec<String> }

/// Round-trip cost of one position in bps: two commissions over the position size plus two half-spreads.
pub fn round_trip_bps(costs: &Costs, symbol: &str, size: f64) -> f64 { 2.0 * costs.one_way(symbol, size) * 1e4 }

/// The bot holds `positions` positions at a time and ANY symbol whose signal fires first takes a slot (owner), so there is no portfolio to pick. For every stock the model
/// ranks, this answers: if its signal fires, take it or skip it? Take = the market regime allows entries, the model's recent record is not significantly bad, and the
/// calibrated expected return beats the stock's measured round-trip cost at the position size. Size = a slot (account / positions), halved in a high-volatility regime and
/// scaled down for stocks whose own tail is wilder (`scale`, from `portfolio::slot_scale`). Trust comes from the model's live record.
#[allow(clippy::too_many_arguments)]
pub fn assess(picks: &[Pick], costs: &Costs, account: f64, positions: usize, spy_trend_up: bool, vol_pct: Option<f64>, record: &Scoreboard, recent_ic: Option<(f64, f64, usize)>, scale: &BTreeMap<String, f64>) -> Assessment {
    let mut notes = vec![];
    let stance = if !spy_trend_up { notes.push("SPY is below its 130-bar average: the bot's own trend filter stays out".to_string()); Stance::StandAside }
        else if vol_pct.is_some_and(|p| p >= 0.8) { notes.push(format!("forecast volatility is higher than {:.0}% of the history: half-size slots", vol_pct.unwrap_or(0.0) * 100.0)); Stance::Reduced }
        else { Stance::RiskOn };
    let bad = recent_ic.is_some_and(|(m, se, n)| n >= 10 && m + 1.96 * se < 0.0);
    let trust = if bad { "LOW: the model's recent out-of-sample record is significantly negative".to_string() }
        else if record.moments >= 20 && record.ic > 0.0 && record.ic_se.is_finite() && record.ic - 1.96 * record.ic_se > 0.0 { format!("PROVEN LIVE: rank IC {:+.3} over {} scored rankings", record.ic, record.moments) }
        else if record.moments >= 5 { format!("UNPROVEN: {} scored rankings, rank IC {:+.3} (not yet significant)", record.moments, record.ic) }
        else { "UNPROVEN: the model has fewer than 5 scored live rankings".to_string() };
    if bad { notes.push("abstaining: skip every signal until the model's record recovers".to_string()); }
    let allowed = stance != Stance::StandAside && !bad;
    let base = account / positions as f64 * if stance == Stance::Reduced { 0.5 } else { 1.0 };
    let calls = picks.iter().map(|p| {
        let size = (base * scale.get(&p.symbol).copied().unwrap_or(1.0)).round();
        let net = p.expected_bps - round_trip_bps(costs, &p.symbol, size.max(1.0));
        Take { symbol: p.symbol.clone(), take: allowed && net > 0.0, pct: p.pct, expected_bps: p.expected_bps, net_bps: net, size_usd: size }
    }).collect();
    Assessment { stance, trust, calls, notes }
}

/// The full trading briefing for the newest bar: regime, risk forecast, the model's ranking after costs, the decision and the confidence, as plain text.
/// Also runs and records the two honest tests (volatility forecast and direction) in the trial ledger.
pub fn briefing(root: &std::path::Path, db: &rusqlite::Connection, series: &BTreeMap<String, Vec<Bar>>) -> anyhow::Result<String> {
    let spy = series.get("SPY").ok_or_else(|| anyhow::anyhow!("no SPY candles"))?;
    let (labelled, live) = (observations(spy, false), observations(spy, true));
    let now = live.last().ok_or_else(|| anyhow::anyhow!("not enough SPY history"))?;
    crate::mlimprove::init(db)?;
    let _ = std::fs::write(root.join("model").join("vol_track.json"), serde_json::to_vec(&volatility_track(&labelled)).unwrap_or_default());
    let mut t = String::new();
    if let Some(r) = test_volatility(&labelled) {
        let gain = (1.0 - r.mse_model / r.mse_naive) * 100.0;
        t += &format!("RISK MODEL (SPY volatility over the next session, out of sample): forecast error {gain:.1}% below the naive forecast (squared-error gain interval [{:+.4},{:+.4}] over {} weeks), correlation {:.2} vs {:.2} naive -> {}\n",
            r.gain_lo, r.gain_hi, r.windows, r.corr_model, r.corr_naive, if r.gain_lo > 0.0 { "it IMPROVES the naive forecast" } else { "no proven improvement over the naive forecast" });
        crate::mlimprove::record(db, "risk", "SPY volatility forecast vs naive", "sq_error_gain", r.mse_naive - r.mse_model, r.gain_lo, r.gain_hi, r.gain_lo > 0.0)?;
    }
    if let Some(d) = test_direction(&labelled) {
        t += &format!("DIRECTION MODEL (will SPY be higher after the next session): out-of-sample AUC {:.3} [{:.3},{:.3}] over {} hourly observations -> {}\n", d.auc, d.lo, d.hi, d.n, if d.lo > 0.5 { "SKILL" } else { "no skill (as expected for a random-walk-like index); direction is NOT predicted" });
        crate::mlimprove::record(db, "risk", "SPY direction (AUC)", "auc", d.auc, d.lo, d.hi, d.lo > 0.5)?;
    }
    let fc = forecast_now(&labelled, now);
    let trend_up = now.x[5] > 0.0;
    t += &format!("REGIME: SPY {:.2}% {} its 130-bar average, one-session return {:+.2}%{}\n", now.x[5].abs() * 100.0, if trend_up { "above" } else { "BELOW" }, now.x[3] * 100.0,
        fc.map_or(String::new(), |(v, p)| format!("; forecast volatility for the next session {:.2}% (higher than {:.0}% of the history)", v * 100.0, p * 100.0)));
    let Some(cur) = crate::aimodel::current(root).and_then(|v| crate::aimodel::load(root, v).ok()) else { return Ok(t + "no trained model yet\n") };
    let Some((ts, picks)) = crate::aimodel::predict_live(series, &cur, &crate::automl::load_champions(root)) else { return Ok(t + "not enough stocks with features at the newest bar\n") };
        let table: Option<BTreeMap<String, Vec<(i64, f64)>>> = std::fs::read(crate::brain_agents::scores_path()).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let recent = table.as_ref().and_then(|tb| crate::brain_agents::recent_ic(series, tb, 15));
    let record = crate::aimodel::scoreboard(db, None)?;
    // each stock's own real one-session returns (hourly moments, newest 400): the tail that sizes its slot
    let mut own: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for s in crate::mlcandle::build(series, 4).iter().rev() {
        let name = series.keys().nth(s.sym).cloned().unwrap_or_default();
        if s.raw.is_finite() && picks.iter().any(|p| p.symbol == name) { let e = own.entry(name).or_default(); if e.len() < 400 { e.push(s.raw); } }
    }
    let mut tails: Vec<f64> = own.values().filter(|v| v.len() >= 50).map(|v| crate::portfolio::cvar(&v.iter().map(|r| vec![*r]).collect::<Vec<_>>(), &[1.0], 0.9)).collect();
    tails.sort_by(f64::total_cmp);
    let reference = tails.get(tails.len() / 2).copied().unwrap_or(0.0);
    let scale: BTreeMap<String, f64> = own.iter().map(|(k, v)| (k.clone(), crate::portfolio::slot_scale(v, reference))).collect();
    let d = assess(&picks, &crate::candles::load_costs(db), crate::mlwalk::ACCOUNT, 2, trend_up, fc.map(|x| x.1), &record, recent, &scale);
    t += &format!("MODEL: version {} ranking for the bar {}; record: {}\n", cur.version, crate::util::utc_minute(ts / 1000), record.line());
    t += &format!("STANCE: {:?}. CONFIDENCE: {}\n", d.stance, d.trust);
    t += "RULE: 2 positions at a time; the first symbols whose signals fire take the slots. The model only says which signals to accept.\n";
    let list = |take: bool| -> String { d.calls.iter().filter(|c| c.take == take).map(|c| if take { format!("{} ({:.0} USD, expected {:+.0} bps, {:+.0} after costs)", c.symbol, c.size_usd, c.expected_bps, c.net_bps) } else { format!("{} ({:+.0} bps expected, {:+.0} after costs)", c.symbol, c.expected_bps, c.net_bps) }).collect::<Vec<_>>().join("; ") };
    t += &format!("DECISION take if its signal fires: {}\nDECISION skip if its signal fires: {}\n", list(true), list(false));
    for n in &d.notes { t += &format!("NOTE: {n}\n"); }
    t += "Advisory only: the bot trades by its own searched strategy; this briefing places no order.\n";
    Ok(t)
}

/// The model as a filter on the bot's REAL entries: every backtest entry (the unselected runs) is looked up in the walk-forward score file at the bar before its decision
/// (a score from a model fitted on older data), given its percentile among the stocks scored at that bar, and the trades the model would have skipped (percentile below
/// 20 / 35 / 50) are compared with all trades by block-resampled interval. This is the question the bot's rule poses: a signal fired, does the model say take it?
pub fn gate_on_trades(db: &rusqlite::Connection, data: &rusqlite::Connection) -> anyhow::Result<String> {
    use crate::mltrade::{judge, Trade};
    let table: BTreeMap<String, Vec<(i64, f64)>> = serde_json::from_slice(&std::fs::read(crate::brain_agents::scores_path())?)?;
    if table.is_empty() { return Ok("trade-filter test: the score file is empty (the model is switched off)".into()); }
    let mut at: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for v in table.values() { for (ts, sc) in v { at.entry(*ts).or_default().push(*sc); } }
    for v in at.values_mut() { v.sort_by(f64::total_cmp); }
    let mut st = data.prepare("SELECT r.window_start, t.symbol, t.decided_ts, t.ret_margin FROM trades t JOIN runs r ON r.run_id = t.run_id WHERE r.role IN ('random', 'oos') AND t.side = 'long' AND t.decided_ts IS NOT NULL")?;
    let (mut trades, mut pcts): (Vec<Trade>, Vec<f64>) = (vec![], vec![]);
    for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, f64>(3)?)))?.flatten() {
        let ts = row.2 * 1000 - 900_000;
        let Some(v) = table.get(&row.1) else { continue };
        let Ok(i) = v.binary_search_by_key(&ts, |x| x.0) else { continue };
        let all = &at[&ts];
        if all.len() < 10 { continue; }
        pcts.push(100.0 * all.iter().filter(|x| **x < v[i].1).count() as f64 / all.len() as f64);
        trades.push(Trade { block: row.0, x: vec![], win: row.3 > 0.0, ret: row.3 });
    }
    if trades.len() < 500 { return Ok(format!("trade-filter test: only {} backtest entries have a score", trades.len())); }
    crate::mlimprove::init(db)?;
    let mut t = format!("MODEL AS A FILTER ON THE BOT'S OWN ENTRIES ({} backtest entries scored by walk-forward models; fees are inside each trade's return):\n", trades.len());
    for thr in [20.0, 35.0, 50.0] {
        let v = judge(&format!("skip below the {thr:.0}th percentile"), &trades, &pcts, thr, 5);
        t += &format!("  skip the lowest {:.0}%: keeps {:.0}% of entries | mean return per trade {:+.3} pts -> {:+.3} pts ({:+.3} pts [{:+.3},{:+.3}] 95%) | win rate {:.1}% -> {:.1}% | better in {} of {} blocks\n",
            thr, v.keep_share * 100.0, v.ret_all * 100.0, v.ret_kept * 100.0, (v.ret_kept - v.ret_all) * 100.0, v.diff_lo * 100.0, v.diff_hi * 100.0, v.win_all * 100.0, v.win_kept * 100.0, v.blocks_better, v.blocks);
        crate::mlimprove::record(db, "gate", &format!("skip below the {thr:.0}th percentile (on backtest entries)"), "pts_per_trade", (v.ret_kept - v.ret_all) * 100.0, v.diff_lo * 100.0, v.diff_hi * 100.0, v.diff_lo > 0.0)?;
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(sym: &str, pct: f64, bps: f64) -> Pick { Pick { symbol: sym.into(), score: 0.0, pct, expected_bps: bps, hit: 0.5, why: vec![] } }
    fn costs() -> Costs { Costs { commission: 1.0, half: BTreeMap::new(), default_half: 0.0003 } }

    #[test]
    fn each_signal_is_taken_only_when_the_expected_return_beats_the_cost_and_sized_by_the_stocks_own_tail() {
        let picks = vec![pick("AAA", 100.0, 40.0), pick("BBB", 95.0, 12.0), pick("CCC", 90.0, 25.0), pick("DDD", 20.0, -30.0)];
        let scale: BTreeMap<String, f64> = [("CCC".to_string(), 0.5)].into_iter().collect();
        // 2 positions of 2,500 USD: a round trip costs 2 x (1/2500 + 3 bps) = 14 bps; CCC's slot is halved to 1,250 USD: 2 x (1/1250 + 3 bps) = 22 bps
        let d = assess(&picks, &costs(), 5000.0, 2, true, Some(0.3), &Scoreboard::default(), None, &scale);
        assert_eq!(d.stance, Stance::RiskOn);
        let call = |s: &str| d.calls.iter().find(|c| c.symbol == s).unwrap().clone();
        assert!(call("AAA").take && (call("AAA").net_bps - 26.0).abs() < 0.5 && call("AAA").size_usd == 2500.0);
        assert!(!call("BBB").take, "12 bps does not beat the 14 bps cost");
        assert!(call("CCC").take && call("CCC").size_usd == 1250.0 && (call("CCC").net_bps - 3.0).abs() < 0.5, "{:?}", call("CCC"));
        assert!(!call("DDD").take, "a negative expected return is always skipped");
        assert!(d.trust.starts_with("UNPROVEN"));
    }

    #[test]
    fn it_stands_aside_in_a_downtrend_halves_slots_in_high_volatility_and_abstains_when_the_record_is_bad() {
        let picks = vec![pick("AAA", 100.0, 60.0)];
        let sb = Scoreboard::default();
        let v = BTreeMap::new();
        assert!(!assess(&picks, &costs(), 5000.0, 2, false, Some(0.1), &sb, None, &v).calls[0].take);
        let hi = assess(&picks, &costs(), 5000.0, 2, true, Some(0.95), &sb, None, &v);
        assert_eq!((hi.stance, hi.calls[0].size_usd, hi.calls[0].take), (Stance::Reduced, 1250.0, true));
        let bad = assess(&picks, &costs(), 5000.0, 2, true, Some(0.1), &sb, Some((-0.1, 0.02, 15)), &v);
        assert!(!bad.calls[0].take && bad.trust.starts_with("LOW") && bad.notes.iter().any(|n| n.contains("abstaining")));
        let proven = Scoreboard { moments: 30, calls: 600, ic: 0.08, ic_se: 0.02, ..Default::default() };
        assert!(assess(&picks, &costs(), 5000.0, 2, true, Some(0.1), &proven, None, &v).trust.starts_with("PROVEN"));
        assert!(round_trip_bps(&costs(), "X", 500.0) > round_trip_bps(&costs(), "X", 2500.0), "small positions pay more");
    }

    /// 400 sessions of SPY with volatility clustering (GARCH-like): yesterday's volatility predicts tomorrow's better than a single reading does.
    fn clustered_spy(sessions: usize) -> Vec<Bar> {
        let mut st = 11u64; let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; ((st >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 3.4 };
        let start = 1_775_000_000_000i64 / DAY_MS * DAY_MS + 13 * 3_600_000 + 30 * 60_000;
        let (mut p, mut vol) = (400.0, 0.002); let mut v = vec![];
        for d in 0..sessions { for k in 0..26 {
            vol = (0.0000004 + 0.85 * vol * vol + 0.12 * (vol * rnd()).powi(2)).sqrt().clamp(0.0004, 0.01);
            let o = p; p *= 1.0 + vol * rnd();
            v.push(Bar { ts: start + ((d / 5) as i64 * 7 + (d % 5) as i64) * DAY_MS + k as i64 * MS15, open: o, high: o.max(p), low: o.min(p), close: p, volume: 1.0 });
        } }
        v
    }

    #[test]
    fn the_volatility_forecast_beats_the_naive_one_on_clustered_data_and_the_direction_model_shows_no_skill_on_a_random_walk() {
        let spy = clustered_spy(420);
        let obs = observations(&spy, false);
        let r = test_volatility(&obs).expect("a report");
        assert!(r.mse_model < r.mse_naive && r.corr_model > r.corr_naive, "{r:?}");
        let d = test_direction(&obs).expect("a report");
        assert!(d.lo < 0.5 + 0.02 && d.hi > 0.5 - 0.02 && d.n > 200, "a random walk has no direction skill: {d:?}");
        // the live observation exists for the newest bar and has no outcome yet
        let live = observations(&spy, true);
        assert!(live.last().unwrap().log_rv.is_nan());
        let (vol, pct) = forecast_now(&obs, live.last().unwrap()).unwrap();
        assert!(vol > 0.0 && (0.0..=1.0).contains(&pct));
    }
}
