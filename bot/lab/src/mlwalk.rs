//! Walk-forward trading test WITH FEES and with new parameters chosen for every 14-session block (owner 2026-10-13).
//!
//! The procedure, for each test block of 14 trading sessions (the lab's block length):
//!   1. the parameters are chosen from data BEFORE the block only: for every candidate (ridge strength, number of positions, entry time, minimum edge) the
//!      strategy is replayed on the three preceding 14-session blocks, each predicted by a model trained only on data older than that block, and the
//!      candidate with the best net return after fees is picked;
//!   2. the model is refitted on everything older than the block (2 sessions of purge, the label looks a session ahead) with the chosen ridge strength;
//!   3. the block is traded with the chosen parameters and nothing else is allowed to change. Block results are pooled.
//!
//! Fees are the bot's measured ones (`candles::Costs`): commission per order (about 1 USD) divided by the position size, plus the symbol's half-spread, on every
//! buy and every sell; a name kept from one day to the next costs nothing. Position size = account / number of positions (5,000 USD).
//! The return is the stock's own return over the next session (not an excess over the universe); a day without a trade earns 0 (cash).
//! A control runs the identical procedure with models trained on labels shuffled within each moment (the returns being earned stay real).

use crate::candles::Costs;
use crate::mlcandle::{Ridge, Sample, FEATURES};
use std::collections::{BTreeMap, BTreeSet};

pub const ACCOUNT: f64 = 5000.0;
pub const BLOCK: usize = 14;
pub const PURGE: usize = 2;
const INNER_BLOCKS: usize = 3;
const FIRST_TEST_SESSION: usize = 56;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params { pub alpha: f64, pub k: usize, pub bar: f64, pub theta_bps: f64 }

pub fn grid() -> Vec<Params> {
    let mut g = vec![];
    for alpha in [10.0, 100.0, 1000.0, 10_000.0] { for k in [2usize, 3, 5, 10] { for bar in [2.0, 6.0, 10.0] { for theta_bps in [0.0, 10.0, 20.0] { g.push(Params { alpha, k, bar, theta_bps }); } } } }
    g
}

fn session_of(ts: i64) -> i64 { (ts + crate::mlcandle::et_offset_ms(ts)).div_euclid(86_400_000) }

#[derive(Debug, Clone)]
pub struct Day { pub session: i64, pub week: i64, pub net: f64, pub gross: f64, pub fee: f64, pub universe: f64, pub traded: bool }

/// Replays the daily strategy over the given samples (real returns in `raw`), scored by `pred` (aligned with `s`).
pub fn simulate(s: &[Sample], pred: &[f64], p: &Params, costs: &Costs, names: &[String]) -> Vec<Day> {
    let bi = FEATURES.iter().position(|f| *f == "bar_in_day").expect("bar_in_day");
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, x) in s.iter().enumerate() { if (x.x[bi] - p.bar).abs() < 1e-9 { by.entry(x.ts).or_default().push(i); } }
    let size = ACCOUNT / p.k as f64;
    let (mut out, mut held): (Vec<Day>, BTreeSet<usize>) = (vec![], BTreeSet::new());
    for v in by.values().filter(|v| v.len() >= crate::mlcandle::MIN_CROSS) {
        let mut o = v.clone(); o.sort_by(|&a, &b| pred[b].total_cmp(&pred[a]));
        let top: Vec<usize> = o.iter().take(p.k).copied().collect();
        let one = |i: usize| costs.one_way(&names[s[i].sym], size);
        let round_trip: f64 = top.iter().map(|&i| 2.0 * one(i)).sum::<f64>() / top.len() as f64;
        let edge = top.iter().map(|&i| pred[i]).sum::<f64>() / top.len() as f64 - round_trip;
        let trade = edge * 1e4 > p.theta_bps;
        let want: BTreeSet<usize> = if trade { top.iter().map(|&i| s[i].sym).collect() } else { BTreeSet::new() };
        // fees: buy every new name, sell every name no longer wanted; kept names cost nothing
        let sym_cost = |sym: usize| costs.one_way(&names[sym], size);
        let fee = (want.difference(&held).map(|&x| sym_cost(x)).sum::<f64>() + held.difference(&want).map(|&x| sym_cost(x)).sum::<f64>()) / p.k as f64;
        held = want;
        let gross = if trade { top.iter().map(|&i| s[i].raw).sum::<f64>() / top.len() as f64 } else { 0.0 };
        let universe = v.iter().map(|&i| s[i].raw).sum::<f64>() / v.len() as f64;
        out.push(Day { session: session_of(s[v[0]].ts), week: s[v[0]].week, net: gross - fee, gross, fee, universe, traded: trade });
    }
    out
}

#[derive(Debug, Clone)]
pub struct BlockResult { pub first: i64, pub last: i64, pub params: Params, pub days: Vec<Day> }

fn mean(v: &[f64]) -> f64 { if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 } }

/// `real` carries the true returns; `learn` is the same samples (same order) with the labels the models are trained on (identical to `real` except in the control).
pub fn walk_forward(real: &[Sample], learn: &[Sample], costs: &Costs, names: &[String], fixed: Option<Params>) -> Vec<BlockResult> {
    let sessions: Vec<i64> = { let mut v: Vec<i64> = real.iter().map(|s| session_of(s.ts)).collect(); v.sort(); v.dedup(); v };
    let sess = |i: usize| session_of(real[i].ts);
    let sidx: BTreeMap<i64, usize> = sessions.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let pos: Vec<usize> = (0..real.len()).map(|i| sidx[&sess(i)]).collect();
    let indices = |lo: usize, hi: usize| -> Vec<usize> { (0..real.len()).filter(|&i| pos[i] >= lo && pos[i] < hi).collect() }; // sessions [lo, hi)
    let take = |idx: &[usize], v: &[Sample]| -> Vec<Sample> { idx.iter().map(|&i| v[i].clone()).collect() };
    let mut out = vec![];
    let mut start = FIRST_TEST_SESSION;
    let g = grid();
    let alphas: Vec<f64> = { let mut a: Vec<f64> = g.iter().map(|p| p.alpha).collect(); a.dedup(); a.sort_by(f64::total_cmp); a.dedup(); a };
    while start + 3 < sessions.len() {
        let end = (start + BLOCK).min(sessions.len());
        let chosen = match fixed {
            Some(p) => p,
            None => {
                // replay the candidates on the INNER_BLOCKS blocks before this one
                let mut score: Vec<(f64, usize)> = vec![(0.0, 0); g.len()];
                for b in 0..INNER_BLOCKS {
                    let ihi = start - (INNER_BLOCKS - 1 - b) * BLOCK; let ilo = ihi - BLOCK;
                    if ilo < PURGE + 10 { continue; }
                    let tr = take(&indices(0, ilo.saturating_sub(PURGE)), learn);
                    let te_idx = indices(ilo, ihi);
                    let te = take(&te_idx, real);
                    if tr.len() < 3000 || te.len() < 500 { continue; }
                    let preds: Vec<(f64, Vec<f64>)> = alphas.iter().map(|&a| { let r = Ridge::fit(&tr, a); (a, te.iter().map(|x| r.predict(x)).collect()) }).collect();
                    for (gi, p) in g.iter().enumerate() {
                        let pr = &preds.iter().find(|x| x.0 == p.alpha).expect("alpha").1;
                        let days = simulate(&te, pr, p, costs, names);
                        score[gi].0 += days.iter().map(|d| d.net).sum::<f64>(); score[gi].1 += days.len();
                    }
                }
                let best = score.iter().enumerate().max_by(|a, b| { let ma = if a.1 .1 == 0 { f64::MIN } else { a.1 .0 / a.1 .1 as f64 }; let mb = if b.1 .1 == 0 { f64::MIN } else { b.1 .0 / b.1 .1 as f64 }; ma.total_cmp(&mb) }).map(|x| x.0).unwrap_or(0);
                g[best]
            }
        };
        let tr = take(&indices(0, start.saturating_sub(PURGE)), learn);
        let te_idx = indices(start, end);
        let te = take(&te_idx, real);
        if tr.len() >= 3000 && te.len() >= 500 {
            let r = Ridge::fit(&tr, chosen.alpha);
            let pr: Vec<f64> = te.iter().map(|x| r.predict(x)).collect();
            out.push(BlockResult { first: sessions[start], last: sessions[end - 1], params: chosen, days: simulate(&te, &pr, &chosen, costs, names) });
        }
        start = end;
    }
    out
}

#[derive(Debug, Clone)]
pub struct Summary { pub days: usize, pub traded: usize, pub net_bps: f64, pub lo: f64, pub hi: f64, pub gross_bps: f64, pub fee_bps: f64, pub universe_bps: f64, pub sharpe: f64, pub up: f64 }

pub fn summarize(days: &[Day]) -> Summary {
    if days.is_empty() { return Summary { days: 0, traded: 0, net_bps: 0.0, lo: 0.0, hi: 0.0, gross_bps: 0.0, fee_bps: 0.0, universe_bps: 0.0, sharpe: 0.0, up: 0.0 }; }
    let net: Vec<f64> = days.iter().map(|d| d.net).collect();
    let m = mean(&net);
    let sd = (net.iter().map(|x| (x - m).powi(2)).sum::<f64>() / net.len() as f64).sqrt().max(1e-12);
    let mut weeks: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for d in days { weeks.entry(d.week).or_default().push(d.net); }
    let wv: Vec<&Vec<f64>> = weeks.values().collect();
    let mut st = 777u64;
    let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; st };
    let mut means: Vec<f64> = (0..2000).map(|_| { let (mut s, mut c) = (0.0, 0usize); for _ in 0..wv.len() { let w = wv[(rnd() % wv.len() as u64) as usize]; s += w.iter().sum::<f64>(); c += w.len(); } s / c.max(1) as f64 }).collect();
    means.sort_by(f64::total_cmp);
    Summary { days: days.len(), traded: days.iter().filter(|d| d.traded).count(), net_bps: m * 1e4, lo: means[50] * 1e4, hi: means[1950] * 1e4,
        gross_bps: mean(&days.iter().map(|d| d.gross).collect::<Vec<_>>()) * 1e4, fee_bps: mean(&days.iter().map(|d| d.fee).collect::<Vec<_>>()) * 1e4,
        universe_bps: mean(&days.iter().map(|d| d.universe).collect::<Vec<_>>()) * 1e4, sharpe: m / sd * 252f64.sqrt(), up: net.iter().filter(|x| **x > 0.0).count() as f64 / net.len() as f64 }
}

fn fmt(name: &str, s: &Summary) -> String {
    format!("  {name:<34} NET {:+.1} bps/day [{:+.1},{:+.1}] | gross {:+.1}, fees {:.1}, buy-and-hold of all names {:+.1} | Sharpe {:+.2}, up days {:.0}%, traded {}/{} days\n",
        s.net_bps, s.lo, s.hi, s.gross_bps, s.fee_bps, s.universe_bps, s.sharpe, s.up * 100.0, s.traded, s.days)
}

pub struct WalkReport { pub text: String, pub pooled: Summary, pub control: Summary, pub forward: Summary, pub forward_days: usize }

/// The full test. `freeze` is the session (New York date index) before which the procedure's design was chosen: blocks starting on or after it are the forward test.
pub fn report(all: &[Sample], names: &[String], costs: &Costs, freeze_session: i64) -> WalkReport {
    let blocks = walk_forward(all, all, costs, names, None);
    let mut t = format!("walk-forward WITH FEES: commission {:.2} USD per order, half-spreads measured per symbol (median {:.1} bps), account {ACCOUNT:.0} USD split over the positions; parameters re-chosen before every {BLOCK}-session block from the previous {INNER_BLOCKS} blocks\n",
        costs.commission, costs.default_half * 1e4);
    for b in &blocks {
        let s = summarize(&b.days);
        t += &format!("  block {}..{}: chose ridge {} | {} positions | enter bar {} | min edge {} bps -> NET {:+.1} bps/day, gross {:+.1}, fees {:.1}, all names {:+.1}, traded {}/{}\n",
            date(b.first), date(b.last), b.params.alpha, b.params.k, b.params.bar as i32, b.params.theta_bps, s.net_bps, s.gross_bps, s.fee_bps, s.universe_bps, s.traded, s.days);
    }
    let days: Vec<Day> = blocks.iter().flat_map(|b| b.days.clone()).collect();
    let pooled = summarize(&days);
    t += "pooled over all blocks (every day out of sample):\n";
    t += &fmt("procedure (params re-chosen per block)", &pooled);
    // the bot's own shape for context: 2 positions, 10:00, no minimum edge, ridge 100 -- fixed, not re-chosen
    let base = summarize(&walk_forward(all, all, costs, names, Some(Params { alpha: 100.0, k: 2, bar: 2.0, theta_bps: 0.0 })).iter().flat_map(|b| b.days.clone()).collect::<Vec<_>>());
    t += &fmt("fixed: 2 positions, 10:00, ridge 100", &base);
    let shuffled = crate::mlcandle::shuffle_within_moment(all, 4242);
    let cdays: Vec<Day> = walk_forward(all, &shuffled, costs, names, None).iter().flat_map(|b| b.days.clone()).collect();
    let control = summarize(&cdays);
    t += &fmt("control: same procedure, shuffled labels", &control);
    let fwd: Vec<Day> = blocks.iter().filter(|b| b.first >= freeze_session).flat_map(|b| b.days.clone()).collect();
    let forward = summarize(&fwd);
    if fwd.len() >= 28 { t += &fmt(&format!("FORWARD (blocks from {})", date(freeze_session)), &forward); } else { t += &format!("  FORWARD test (blocks starting on or after {}): {} sessions so far, needs 28 (two blocks)\n", date(freeze_session), fwd.len()); }
    WalkReport { text: t, pooled, control, forward, forward_days: fwd.len() }
}

pub fn date(session: i64) -> String { chrono::DateTime::from_timestamp(session * 86_400, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default() }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mlcandle::Sample;

    fn costs(comm: f64, half: f64) -> Costs { Costs { commission: comm, half: BTreeMap::new(), default_half: half } }
    fn names(n: usize) -> Vec<String> { (0..n).map(|i| format!("S{i}")).collect() }

    /// One moment per day at bar 2 with 25 names whose next-session return is `rets[i]`; the prediction is the same ranking.
    fn day(ts: i64, rets: &[f64]) -> (Vec<Sample>, Vec<f64>) {
        let mut x = vec![0.0; FEATURES.len()]; x[FEATURES.iter().position(|f| *f == "bar_in_day").unwrap()] = 2.0;
        let s: Vec<Sample> = rets.iter().enumerate().map(|(i, r)| Sample { ts, week: ts / (7 * 86_400_000), sym: i, x: x.clone(), excess: *r, raw: *r }).collect();
        let p = rets.to_vec();
        (s, p)
    }

    #[test]
    fn fees_are_commission_over_size_plus_half_spread_on_every_buy_and_sell() {
        let rets: Vec<f64> = (0..25).map(|i| i as f64 / 1000.0).collect(); // best names return 0.024, 0.023
        let (s, p) = day(1_000_000_000_000, &rets);
        let c = costs(1.0, 0.0002);
        let d = simulate(&s, &p, &Params { alpha: 1.0, k: 2, bar: 2.0, theta_bps: 0.0 }, &c, &names(25));
        assert_eq!(d.len(), 1);
        // 2 positions of 2,500 USD: buying costs (1/2500 + 0.0002) per name = 6 bps; first day only buys
        assert!((d[0].fee - 0.0006).abs() < 1e-12, "{:?}", d[0]);
        assert!((d[0].gross - 0.0235).abs() < 1e-12 && (d[0].net - (0.0235 - 0.0006)).abs() < 1e-12);
    }

    #[test]
    fn names_kept_for_another_day_cost_nothing_and_leaving_costs_a_sell() {
        let rets: Vec<f64> = (0..25).map(|i| i as f64 / 1000.0).collect();
        let (mut s, mut p) = day(1_000_000_000_000, &rets);
        let (s2, p2) = day(1_000_000_000_000 + 86_400_000, &rets);
        s.extend(s2); p.extend(p2);
        let c = costs(1.0, 0.0002);
        let d = simulate(&s, &p, &Params { alpha: 1.0, k: 2, bar: 2.0, theta_bps: 0.0 }, &c, &names(25));
        assert_eq!(d.len(), 2);
        assert!((d[0].fee - 0.0006).abs() < 1e-12 && d[1].fee == 0.0, "same names the next day: no new fees {:?}", d);
        // a minimum edge above the possible edge means no trade: the held names are sold (fee), the day earns nothing
        let (mut s3, mut p3) = day(1_000_000_000_000, &rets);
        let (s4, mut p4) = day(1_000_000_000_000 + 86_400_000, &rets);
        p4.iter_mut().for_each(|x| *x = 0.0);
        s3.extend(s4); p3.extend(p4);
        let d2 = simulate(&s3, &p3, &Params { alpha: 1.0, k: 2, bar: 2.0, theta_bps: 0.0 }, &c, &names(25));
        assert!(!d2[1].traded && d2[1].gross == 0.0 && (d2[1].fee - 0.0006).abs() < 1e-12, "{:?}", d2[1]);
    }

    #[test]
    fn small_positions_make_the_flat_commission_expensive() {
        let rets: Vec<f64> = (0..25).map(|i| i as f64 / 1000.0).collect();
        let (s, p) = day(1_000_000_000_000, &rets);
        let c = costs(1.0, 0.0);
        let few = simulate(&s, &p, &Params { alpha: 1.0, k: 2, bar: 2.0, theta_bps: 0.0 }, &c, &names(25))[0].fee;
        let many = simulate(&s, &p, &Params { alpha: 1.0, k: 10, bar: 2.0, theta_bps: 0.0 }, &c, &names(25))[0].fee;
        assert!((few - 0.0004).abs() < 1e-12 && (many - 0.002).abs() < 1e-12, "2 x 2,500 USD pays 4 bps; 10 x 500 USD pays 20 bps: {few} {many}");
    }

    #[test]
    fn the_walk_forward_never_trains_on_the_block_it_trades_and_a_planted_signal_pays_after_fees() {
        let m: BTreeMap<String, Vec<crate::candles::Bar>> = {
            let mut st = 5u64;
            let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; (st >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
            let start = 1_775_000_000_000i64 / 86_400_000 * 86_400_000 + 13 * 3_600_000 + 30 * 60_000;
            (0..50).map(|s| {
                let mut p = 50.0 + s as f64; let mut v = vec![];
                for i in 0..(26 * 130) { let day = i / 26; let slot = (i % 26) as i64; let ts = start + ((day / 5) as i64 * 7 + (day % 5) as i64) * 86_400_000 + slot * 900_000;
                    let o = p; p *= 1.0 + rnd() * 0.004; v.push(crate::candles::Bar { ts, open: o, high: o.max(p) * 1.001, low: o.min(p) * 0.999, close: p, volume: 1000.0 * (1.0 + rnd() + 0.6) }); }
                (format!("S{s:02}"), v)
            }).collect()
        };
        let nm: Vec<String> = m.keys().cloned().collect();
        let all = crate::mlcandle::build(&m, 4);
        let j = FEATURES.iter().position(|f| *f == "ret_26").unwrap();
        // plant: the next session's return is a strong reversal of the last session's (raw and excess both)
        let planted: Vec<Sample> = all.iter().map(|x| { let r = -0.6 * x.x[j]; Sample { excess: r, raw: r, ..x.clone() } }).collect();
        let c = costs(1.0, 0.0001);
        let blocks = walk_forward(&planted, &planted, &c, &nm, None);
        assert!(!blocks.is_empty());
        let s = summarize(&blocks.iter().flat_map(|b| b.days.clone()).collect::<Vec<_>>());
        assert!(s.net_bps > 20.0 && s.lo > 0.0, "{s:?}");
        // the same procedure on shuffled labels earns nothing worth having
        let sh = crate::mlcandle::shuffle_within_moment(&planted, 3);
        let sc = summarize(&walk_forward(&planted, &sh, &c, &nm, None).iter().flat_map(|b| b.days.clone()).collect::<Vec<_>>());
        assert!(sc.net_bps < s.net_bps / 3.0, "{sc:?} vs {s:?}");
        // no leakage: garbling the training labels from session 98 on cannot change any block that starts on or before session 98
        let mut sess: Vec<i64> = planted.iter().map(|x| session_of(x.ts)).collect(); sess.sort(); sess.dedup();
        let cut = sess[98];
        let spoiled: Vec<Sample> = planted.iter().map(|x| if session_of(x.ts) >= cut { Sample { excess: 9.0, ..x.clone() } } else { x.clone() }).collect();
        let b2 = walk_forward(&planted, &spoiled, &c, &nm, None);
        let early: Vec<&BlockResult> = blocks.iter().filter(|b| b.first <= cut).collect();
        assert!(early.len() >= 3);
        for (a, b) in early.iter().zip(b2.iter()) { assert_eq!(a.params, b.params); assert!(a.days.iter().zip(&b.days).all(|(x, y)| x.net == y.net), "block {} changed when only later labels changed", date(a.first)); }
        // every block starts after the previous one ends and parameters come from the grid
        assert!(blocks.windows(2).all(|w| w[0].last < w[1].first) && blocks.iter().all(|b| grid().contains(&b.params)));
    }
}
