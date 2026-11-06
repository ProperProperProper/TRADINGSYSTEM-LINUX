//! Candidate extra features for the candle model (owner 2026-10-13, item 5). Each is added ALONE to the base model and kept only if the paired,
//! multiple-testing-adjusted test says it helps (`mlimprove`). All use bars up to the decision bar only.
//!   gap          today's open against the previous session's close (earnings and news days show up as gaps)
//!   gap_abs      the size of that gap
//!   fh_vol       volume of today's first hour against its 20-session norm (1 until the first hour is done)
//!   vol_profile  volume so far today against the 20-session norm at the same time of day
//!   beta         the stock's beta to SPY over the previous 20 sessions (15-minute returns)
//!   resid_26     its one-session return minus beta x SPY's (what the market does not explain)
//!   peer_rel_26  its one-session return minus the average of its 5 most correlated peers' (relative strength inside its cluster)

use crate::candles::Bar;
use crate::mlcandle::{Sample, DAY_MS, FEATURES};
use std::collections::BTreeMap;

pub const NAMES: [&str; 7] = ["gap", "gap_abs", "fh_vol", "vol_profile", "beta", "resid_26", "peer_rel_26"];
const PRIOR: usize = 20;

fn session_of(ts: i64) -> i64 { (ts + crate::mlcandle::et_offset_ms(ts)).div_euclid(DAY_MS) }

/// Per bar: (session id, index inside the session, session's first bar index).
fn layout(bars: &[Bar]) -> Vec<(i64, usize, usize)> {
    let (mut out, mut cur, mut start) = (Vec::with_capacity(bars.len()), i64::MIN, 0usize);
    for (i, b) in bars.iter().enumerate() {
        let s = session_of(b.ts);
        if s != cur { cur = s; start = i; }
        out.push((s, i - start, start));
    }
    out
}

pub fn extend(series: &BTreeMap<String, Vec<Bar>>, base: &[Sample]) -> Vec<Sample> {
    let names: Vec<&String> = series.keys().collect();
    let bars: Vec<&Vec<Bar>> = series.values().collect();
    let lay: Vec<Vec<(i64, usize, usize)>> = bars.iter().map(|b| layout(b)).collect();
    let idx: Vec<BTreeMap<i64, usize>> = bars.iter().map(|b| b.iter().enumerate().map(|(i, x)| (x.ts, i)).collect()).collect();
    // cumulative volume by (session, slot) per symbol, for the profile and first-hour norms
    let sessions_of = |s: usize| -> Vec<i64> { let mut v: Vec<i64> = lay[s].iter().map(|l| l.0).collect(); v.dedup(); v };
    let cum: Vec<BTreeMap<i64, Vec<f64>>> = (0..bars.len()).map(|s| {
        let mut m: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
        for (i, b) in bars[s].iter().enumerate() { let e = m.entry(lay[s][i].0).or_default(); let prev = e.last().copied().unwrap_or(0.0); e.push(prev + b.volume); }
        m
    }).collect();
    // returns on the global timeline for betas and correlations
    let mut tl: Vec<i64> = bars.iter().flat_map(|b| b.iter().map(|x| x.ts)).collect(); tl.sort(); tl.dedup();
    let pos: BTreeMap<i64, usize> = tl.iter().enumerate().map(|(i, t)| (*t, i)).collect();
    let rets: Vec<Vec<f64>> = (0..bars.len()).map(|s| {
        let mut r = vec![f64::NAN; tl.len()];
        for i in 1..bars[s].len() { let (a, b) = (&bars[s][i - 1], &bars[s][i]); if pos[&b.ts] == pos[&a.ts] + 1 { r[pos[&b.ts]] = (b.close / a.close).ln(); } }
        r
    }).collect();
    let spy = names.iter().position(|n| n.as_str() == "SPY");
    // first timeline index of each session, to find "the previous 20 sessions" for any session
    let mut sess_first: BTreeMap<i64, usize> = BTreeMap::new();
    for (g, t) in tl.iter().enumerate() { sess_first.entry(session_of(*t)).or_insert(g); }
    let sess_list: Vec<i64> = sess_first.keys().copied().collect();
    let sess_pos: BTreeMap<i64, usize> = sess_list.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    // per session: beta of each symbol to SPY and the 5 most correlated peers, from the previous PRIOR sessions only
    let mut beta_peer: BTreeMap<i64, (Vec<f64>, Vec<Vec<usize>>)> = BTreeMap::new();
    let ensure = |beta_peer: &mut BTreeMap<i64, (Vec<f64>, Vec<Vec<usize>>)>, sess: i64| {
        if beta_peer.contains_key(&sess) { return; }
        let sp = sess_pos[&sess];
        if sp < PRIOR { beta_peer.insert(sess, (vec![1.0; bars.len()], vec![vec![]; bars.len()])); return; }
        let (g0, g1) = (sess_first[&sess_list[sp - PRIOR]], sess_first[&sess]);
        let n = bars.len();
        let cols: Vec<Vec<f64>> = (0..n).map(|s| (g0..g1).map(|g| rets[s][g]).collect()).collect();
        let corr = |a: &[f64], b: &[f64]| -> Option<f64> {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab, mut k) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
            for (x, y) in a.iter().zip(b) { if x.is_finite() && y.is_finite() { sa += x; sb += y; saa += x * x; sbb += y * y; sab += x * y; k += 1.0; } }
            if k < 100.0 { return None; }
            let (va, vb) = (saa / k - (sa / k).powi(2), sbb / k - (sb / k).powi(2));
            if va <= 1e-14 || vb <= 1e-14 { return None; }
            Some((sab / k - sa / k * sb / k) / (va * vb).sqrt())
        };
        let mut beta = vec![1.0; n];
        if let Some(sp_i) = spy {
            for s in 0..n {
                let (mut sa, mut sb, mut sab, mut sbb, mut k) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
                for (x, y) in cols[s].iter().zip(&cols[sp_i]) { if x.is_finite() && y.is_finite() { sa += x; sb += y; sab += x * y; sbb += y * y; k += 1.0; } }
                let var = sbb / k.max(1.0) - (sb / k.max(1.0)).powi(2);
                if k >= 100.0 && var > 1e-14 { beta[s] = (sab / k - sa / k * sb / k) / var; }
            }
        }
        let mut peers = vec![vec![]; n];
        for s in 0..n {
            if Some(s) == spy { continue; }
            let mut c: Vec<(f64, usize)> = (0..n).filter(|&o| o != s && Some(o) != spy).filter_map(|o| corr(&cols[s], &cols[o]).map(|v| (v, o))).collect();
            c.sort_by(|a, b| b.0.total_cmp(&a.0));
            peers[s] = c.iter().take(5).map(|x| x.1).collect();
        }
        beta_peer.insert(sess, (beta, peers));
    };
    let j26 = FEATURES.iter().position(|f| *f == "ret_26").expect("ret_26");
    let mut out = Vec::with_capacity(base.len());
    let mut i = 0;
    while i < base.len() {
        let mut j = i; while j < base.len() && base[j].ts == base[i].ts { j += 1; }
        let ts = base[i].ts;
        let sess = session_of(ts);
        ensure(&mut beta_peer, sess);
        let (beta, peers) = &beta_peer[&sess];
        let r26: BTreeMap<usize, f64> = base[i..j].iter().map(|s| (s.sym, s.x[j26])).collect();
        let spy_r26 = spy.and_then(|sp| idx[sp].get(&ts).and_then(|&k| if k >= 26 { Some((bars[sp][k].close / bars[sp][k - 26].close).ln()) } else { None })).unwrap_or(0.0);
        for s in &base[i..j] {
            let Some(&k) = idx[s.sym].get(&ts) else { out.push(Sample { x: { let mut x = s.x.clone(); x.extend([0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0]); x }, ..s.clone() }); continue };
            let (sid, slot, first) = lay[s.sym][k];
            // gap against the previous session's last close
            let gap = if first > 0 { bars[s.sym][first].open / bars[s.sym][first - 1].close - 1.0 } else { 0.0 };
            let gap = if (-0.5..=0.5).contains(&gap) { gap } else { 0.0 };
            // volume norms from the previous PRIOR sessions at the same slot
            let sl = sessions_of(s.sym);
            let me = sl.iter().position(|x| *x == sid).unwrap_or(0);
            let prior: Vec<&Vec<f64>> = sl[me.saturating_sub(PRIOR)..me].iter().filter_map(|x| cum[s.sym].get(x)).filter(|c| c.len() > slot).collect();
            let today = &cum[s.sym][&sid];
            let profile = if prior.len() >= 5 { let norm = prior.iter().map(|c| c[slot]).sum::<f64>() / prior.len() as f64; if norm > 0.0 { today[slot] / norm } else { 1.0 } } else { 1.0 };
            let fh = if slot >= 3 && prior.len() >= 5 { let norm = prior.iter().map(|c| c[3]).sum::<f64>() / prior.len() as f64; if norm > 0.0 { today[3] / norm } else { 1.0 } } else { 1.0 };
            let b = beta[s.sym];
            let own = s.x[j26];
            let peer = &peers[s.sym];
            let peer_rel = if peer.is_empty() { 0.0 } else { let v: Vec<f64> = peer.iter().filter_map(|p| r26.get(p).copied()).collect(); if v.is_empty() { 0.0 } else { own - v.iter().sum::<f64>() / v.len() as f64 } };
            let mut x = s.x.clone();
            x.extend([gap, gap.abs(), fh, profile, b, own - b * spy_r26, peer_rel]);
            out.push(Sample { x, ..s.clone() });
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` sessions of 26 bars; symbol closes follow `f(symbol, bar)`; every session opens with `gap_mult` x the previous close for symbol `gap_sym`.
    fn market(n_sym: usize, sessions: usize, seed: u64, f: &dyn Fn(usize, usize, f64) -> f64) -> BTreeMap<String, Vec<Bar>> {
        let mut st = seed | 1;
        let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; (st >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let start = 1_775_000_000_000i64 / DAY_MS * DAY_MS + 13 * 3_600_000 + 30 * 60_000;
        let common: Vec<f64> = (0..sessions * 26).map(|_| rnd() * 0.004).collect();
        let mut m = BTreeMap::new();
        for s in 0..n_sym {
            let mut p = 100.0; let mut v = vec![];
            for i in 0..sessions * 26 {
                let day = i / 26; let ts = start + ((day / 5) as i64 * 7 + (day % 5) as i64) * DAY_MS + (i % 26) as i64 * 900_000;
                let o = f(s, i, p); p = o * (1.0 + common[i] + rnd() * 0.002);
                v.push(Bar { ts, open: o, high: o.max(p) * 1.001, low: o.min(p) * 0.999, close: p, volume: 1000.0 * (1.0 + (i % 26) as f64 * 0.1) });
            }
            m.insert(if s == 0 { "SPY".to_string() } else { format!("S{s:02}") }, v);
        }
        m
    }

    #[test]
    fn features_are_appended_and_never_look_ahead() {
        let m = market(25, 60, 3, &|_, _, p| p);
        let base = crate::mlcandle::build(&m, 4);
        let ext = extend(&m, &base);
        assert_eq!(ext.len(), base.len());
        assert!(ext.iter().all(|s| s.x.len() == FEATURES.len() + NAMES.len() && s.x.iter().all(|v| v.is_finite())));
        // rewrite the last 15 sessions: features of earlier decisions must not move
        let mut m2 = m.clone();
        for b in m2.values_mut() { let n = b.len(); for x in &mut b[n - 15 * 26..] { x.close *= 1.7; x.open *= 1.7; x.high *= 1.7; x.low *= 1.7; x.volume *= 4.0; } }
        let base2 = crate::mlcandle::build(&m2, 4);
        let ext2 = extend(&m2, &base2);
        let cutoff = m["S01"][m["S01"].len() - 15 * 26 - 40].ts;
        let a: Vec<&Sample> = ext.iter().filter(|s| s.ts <= cutoff).collect();
        let b: Vec<&Sample> = ext2.iter().filter(|s| s.ts <= cutoff).collect();
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) { assert_eq!(x.x, y.x, "a feature changed when only the future was rewritten"); }
    }

    #[test]
    fn the_gap_is_the_open_against_the_previous_close() {
        // symbol S05 jumps +3% at every session open; S06 follows S05's returns exactly (so they are each other's best peer)
        let m = market(25, 60, 9, &|s, i, p| if s == 5 && i % 26 == 0 && i > 0 { p * 1.03 } else { p });
        let base = crate::mlcandle::build(&m, 4);
        let ext = extend(&m, &base);
        let g = FEATURES.len();
        let s5 = ext.iter().rfind(|s| s.sym == 4 && s.x[FEATURES.iter().position(|f| *f == "bar_in_day").unwrap()] == 2.0).unwrap();
        assert!((s5.x[g] - 0.03).abs() < 0.01, "gap {}", s5.x[g]);
        assert!((s5.x[g + 1] - s5.x[g].abs()).abs() < 1e-12);
        let s9 = ext.iter().rfind(|s| s.sym == 8).unwrap();
        assert!(s9.x[g].abs() < 0.01, "a symbol without a planted jump has no big gap");
    }
}
