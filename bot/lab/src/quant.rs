//! Quant review of the strategy tests (owner, 2026-10-11: "make this think like a quant trader and carefully analyse and learn
//! from and apply things testing"). Computed by code from the per-block results (strategy_blocks), never by a model.
//!
//! What a quant asks of every test, and what the averages on the dashboard did not answer:
//! - Is the gain real? Variant and current strategy are compared block by block on the SAME blocks (paired): mean difference,
//!   its t statistic, a bootstrap 95% interval and the share of blocks that were better.
//! - Does it survive selection? ~200 variants were tried; the best of N noisy tries looks good by chance. A gain counts only if
//!   its t beats the expected maximum of N null t values (sqrt(2 ln N)) and its Benjamini-Hochberg q is below 0.10.
//! - Is it worth the risk? Return per block divided by its volatility (a per-block Sharpe), worst block, profit factor.
//! - Is it overfitted? How much of the training-half gain is left on the unseen half, per option family.
//! - Which options carry edge? Every option's paired gain over all variants that used it.
//!
//! The view is computed for one half of the blocks: TRAINING (first half: what the agents and the planner may use, so choosing
//! what to test never leaks into the verdict) or UNSEEN (second half: the human dashboard). The verdict rules themselves are
//! unchanged (strategy::considerably_better and the full validation decide).

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Which half of each variant's cut-A blocks a review uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Half {
    /// The first half (the agents' and the planner's view).
    Training,
    /// The second half (unseen by any agent; the human view).
    Unseen,
}

/// Paired statistics of one variant against the current strategy on the same blocks.
#[derive(Clone, Debug)]
pub struct Paired {
    pub tokens: String,
    pub n: usize,
    pub mean_diff: f64,
    pub t: f64,
    pub ci: (f64, f64),
    pub better_share: f64,
    pub mean: f64,
    pub sharpe: f64,
    pub worst: f64,
    pub base_sharpe: f64,
    pub base_worst: f64,
    pub profit_factor: Option<f64>,
    pub hit_rate: Option<f64>,
    pub q: f64,
}

/// Mean and sample standard deviation.
fn mean_sd(x: &[f64]) -> (f64, f64) {
    let n = x.len() as f64;
    if x.is_empty() { return (0.0, 0.0); }
    let m = x.iter().sum::<f64>() / n;
    let v = if x.len() > 1 { x.iter().map(|a| (a - m).powi(2)).sum::<f64>() / (n - 1.0) } else { 0.0 };
    (m, v.sqrt())
}

/// One-sided p value of a t statistic (normal approximation; blocks are few, so it is a screen, not a proof).
fn p_one_sided(t: f64) -> f64 {
    1.0 - crate::stats::normal_cdf(t)
}

/// Bootstrap 95% interval of the mean (fixed seed: the same data gives the same interval).
fn bootstrap_ci(x: &[f64]) -> (f64, f64) {
    if x.len() < 2 { return (f64::NAN, f64::NAN); }
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state };
    let mut means: Vec<f64> = (0..2000).map(|_| (0..x.len()).map(|_| x[(next() % x.len() as u64) as usize]).sum::<f64>() / x.len() as f64).collect();
    means.sort_by(f64::total_cmp);
    (means[50], means[1949])
}

/// Per-block returns (and trades, wins) of a variant on cut A, in block order.
fn blocks(mem: &Connection, iv: &str, tokens: &str) -> Vec<(i64, f64, i64, i64)> {
    mem.prepare("SELECT start, ret, trades, wins FROM strategy_blocks WHERE interval = ?1 AND tokens = ?2 AND cut = 'A' ORDER BY idx")
        .and_then(|mut st| st.query_map([iv, tokens], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>())
        .unwrap_or_default()
}

/// The chosen half of a block list.
fn half<T: Clone>(b: &[T], h: Half) -> Vec<T> {
    let m = b.len() / 2;
    match h { Half::Training => b[..m].to_vec(), Half::Unseen => b[m..].to_vec() }
}

/// Paired statistics of every tested variant on interval `iv` (with Benjamini-Hochberg q values over all of them).
pub fn paired_all(mem: &Connection, iv: &str, h: Half) -> Result<Vec<Paired>> {
    let base = blocks(mem, iv, "baseline");
    if base.is_empty() { return Ok(vec![]); }
    let base_by: BTreeMap<i64, f64> = half(&base, h).into_iter().map(|b| (b.0, b.1)).collect();
    let base_rets: Vec<f64> = base_by.values().copied().collect();
    let (bm, bs) = mean_sd(&base_rets);
    let base_worst = base_rets.iter().copied().fold(f64::INFINITY, f64::min);
    let toks: Vec<String> = mem.prepare("SELECT DISTINCT tokens FROM strategy_blocks WHERE interval = ?1 AND cut = 'A' AND tokens != 'baseline'")?
        .query_map([iv], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for t in toks {
        let b = half(&blocks(mem, iv, &t), h);
        let pairs: Vec<(f64, f64, i64, i64)> = b.iter().filter_map(|x| base_by.get(&x.0).map(|bb| (x.1, *bb, x.2, x.3))).collect();
        if pairs.len() < 4 { continue; }
        let diffs: Vec<f64> = pairs.iter().map(|p| p.0 - p.1).collect();
        let rets: Vec<f64> = pairs.iter().map(|p| p.0).collect();
        let (md, sd) = mean_sd(&diffs);
        let (m, s) = mean_sd(&rets);
        let tstat = if sd > 0.0 { md / (sd / (diffs.len() as f64).sqrt()) } else { 0.0 };
        let gains: f64 = rets.iter().filter(|r| **r > 0.0).sum();
        let losses: f64 = -rets.iter().filter(|r| **r < 0.0).sum::<f64>();
        let (tr, wn): (i64, i64) = pairs.iter().fold((0, 0), |a, p| (a.0 + p.2, a.1 + p.3));
        out.push(Paired {
            tokens: t, n: diffs.len(), mean_diff: md, t: tstat, ci: bootstrap_ci(&diffs),
            better_share: diffs.iter().filter(|d| **d > 0.0).count() as f64 / diffs.len() as f64,
            mean: m, sharpe: if s > 0.0 { m / s } else { 0.0 }, worst: rets.iter().copied().fold(f64::INFINITY, f64::min),
            base_sharpe: if bs > 0.0 { bm / bs } else { 0.0 }, base_worst,
            profit_factor: if losses > 0.0 { Some(gains / losses) } else { None },
            hit_rate: if tr > 0 { Some(wn as f64 / tr as f64) } else { None }, q: 1.0,
        });
    }
    // Benjamini-Hochberg over every variant reviewed (multiple testing)
    let n = out.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|a, b| p_one_sided(out[*a].t).total_cmp(&p_one_sided(out[*b].t)));
    let mut prev = 1.0f64;
    for (rank, &i) in idx.iter().enumerate().rev() {
        let q = (p_one_sided(out[i].t) * n as f64 / (rank + 1) as f64).min(prev).min(1.0);
        out[i].q = q;
        prev = q;
    }
    out.sort_by(|a, b| b.t.total_cmp(&a.t));
    Ok(out)
}

/// Variants whose paired evidence clears the multiplicity screen. Keeping this rule here
/// lets planners reuse the exact training-only shortlist shown by `review`, without reading
/// the holdout-derived candidate/validated status from the strategy leaderboard.
fn survivors(ps: &[Paired]) -> Vec<&Paired> {
    let bar = (2.0 * (ps.len() as f64).ln()).sqrt();
    ps.iter().filter(|p| p.t > bar && p.q < 0.10).collect()
}

/// Option tokens carried by variants that survive the paired, multiple-testing-adjusted
/// TRAINING-half screen. This is selection evidence for the research planners, never a
/// promotion verdict.
pub fn training_survivor_options(mem: &Connection) -> Result<Vec<String>> {
    let ps = paired_all(mem, "5h", Half::Training)?;
    Ok(survivors(&ps).into_iter()
        .flat_map(|p| p.tokens.split('+').map(String::from))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter().collect())
}

/// Every option's paired gain over the variants that used it: (option, variants, mean of their mean gains, t over variants).
pub fn option_edges(ps: &[Paired]) -> Vec<(String, usize, f64, f64)> {
    let mut by: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for p in ps {
        for o in p.tokens.split('+') {
            let fam = o.split('=').next().unwrap_or(o).to_string();
            by.entry(fam).or_default().push(p.mean_diff);
        }
    }
    let mut v: Vec<(String, usize, f64, f64)> = by.into_iter().filter(|(_, x)| x.len() >= 2).map(|(o, x)| {
        let (m, s) = mean_sd(&x);
        (o, x.len(), m, if s > 0.0 { m / (s / (x.len() as f64).sqrt()) } else { 0.0 })
    }).collect();
    v.sort_by(|a, b| b.3.total_cmp(&a.3));
    v
}

/// The quant review as text and JSON for one half (see the module docs).
pub fn review(mem: &Connection, h: Half) -> Result<(String, Value)> {
    let ps = paired_all(mem, "5h", h)?;
    let label = match h { Half::Training => "TRAINING half (what agents and the planner may use)", Half::Unseen => "UNSEEN half (no agent saw it)" };
    if ps.is_empty() {
        return Ok((format!("QUANT REVIEW ({label}): no per-block results yet"), json!({"variants": 0})));
    }
    let n = ps.len();
    let bar = (2.0 * (n as f64).ln()).sqrt();
    let survivors = survivors(&ps);
    let tested_total: i64 = mem.query_row("SELECT count(DISTINCT tokens) FROM strategy_runs WHERE interval = '5h' AND cut = 'A' AND tokens != 'baseline'", [], |r| r.get(0)).unwrap_or(0);
    let f = |p: &Paired| format!("{} gain {:+.2}/block (95% {:+.2} to {:+.2}), t {:.2}, q {:.2}, better in {:.0}% of {} blocks; Sharpe/block {:.2} vs {:.2}, worst {:+.1}% vs {:+.1}%{}{}",
        p.tokens, p.mean_diff, p.ci.0, p.ci.1, p.t, p.q, p.better_share * 100.0, p.n, p.sharpe, p.base_sharpe, p.worst, p.base_worst,
        p.profit_factor.map_or(String::new(), |x| format!(", profit factor {x:.2}")), p.hit_rate.map_or(String::new(), |x| format!(", trade hit rate {:.0}%", x * 100.0)));
    let mut lines = vec![format!("QUANT REVIEW ({label}; 5h, {n} variants with per-block results of {tested_total} tested; points per 14-session block):")];
    lines.push(format!("  Selection bar: with {n} variants tried, the best t of pure noise is about {bar:.2}; a real edge needs t above it AND q < 0.10."));
    lines.push(if survivors.is_empty() {
        format!("  VERDICT: no variant's gain survives multiple testing (best t {:.2} < {bar:.2}): the apparent winners are consistent with luck from trying many variants. Keep the current strategy; test fewer, better-motivated ideas.", ps[0].t)
    } else {
        format!("  VERDICT: {} variant(s) survive multiple testing: {}", survivors.len(), survivors.iter().map(|p| p.tokens.as_str()).collect::<Vec<_>>().join(", "))
    });
    lines.push("  Strongest paired evidence:".into());
    lines.extend(ps.iter().take(5).map(|p| format!("    {}", f(p))));
    // risk-adjusted: better Sharpe AND shallower worst block than the current strategy
    let mut safer: Vec<&Paired> = ps.iter().filter(|p| p.sharpe > p.base_sharpe && p.worst > p.base_worst).collect();
    safer.sort_by(|a, b| b.sharpe.total_cmp(&a.sharpe));
    lines.push(format!("  Better risk-adjusted (higher Sharpe/block AND shallower worst block than the current strategy {:.2} / {:+.1}%): {}",
        ps[0].base_sharpe, ps[0].base_worst,
        if safer.is_empty() { "none".into() } else { safer.iter().take(5).map(|p| format!("{} ({:.2}, {:+.1}%)", p.tokens, p.sharpe, p.worst)).collect::<Vec<_>>().join(", ") }));
    let edges = option_edges(&ps);
    lines.push("  Option edge over every variant that used it (mean paired gain, t across variants):".into());
    lines.extend(edges.iter().take(6).map(|e| format!("    {} in {} variants: {:+.2}/block, t {:.2}", e.0, e.1, e.2, e.3)));
    if let Some(w) = edges.last() { lines.push(format!("    weakest: {} in {} variants: {:+.2}/block, t {:.2}", w.0, w.1, w.2, w.3)); }
    // the unseen half is human-only; the training view must never carry it
    if h == Half::Unseen { lines.push(persistence_line(mem)?); }
    let js = json!({"half": format!("{h:?}"), "variants": n, "tested": tested_total, "bar": bar, "survivors": survivors.iter().map(|p| &p.tokens).collect::<Vec<_>>(),
        "top": ps.iter().take(10).map(|p| json!({"tokens": p.tokens, "gain": p.mean_diff, "ci": [p.ci.0, p.ci.1], "t": p.t, "q": p.q, "better_share": p.better_share,
            "sharpe": p.sharpe, "base_sharpe": p.base_sharpe, "worst": p.worst, "base_worst": p.base_worst, "profit_factor": p.profit_factor, "hit_rate": p.hit_rate, "blocks": p.n})).collect::<Vec<_>>(),
        "edges": edges.iter().map(|e| json!({"option": e.0, "variants": e.1, "gain": e.2, "t": e.3})).collect::<Vec<_>>()});
    Ok((lines.join("\n"), js))
}

/// Overfitting decay per option family: for variants with per-block results, training-half gain vs unseen-half gain (paired,
/// against the current strategy). A family whose training gains vanish on unseen blocks is fitting noise. Human view only.
pub fn decay(mem: &Connection) -> Result<Vec<(String, usize, f64, f64)>> {
    let tr = paired_all(mem, "5h", Half::Training)?;
    let un: BTreeMap<String, f64> = paired_all(mem, "5h", Half::Unseen)?.into_iter().map(|p| (p.tokens, p.mean_diff)).collect();
    let mut by: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for p in &tr {
        let Some(u) = un.get(&p.tokens) else { continue };
        for o in p.tokens.split('+') {
            let e = by.entry(o.split('=').next().unwrap_or(o).to_string()).or_default();
            e.0.push(p.mean_diff);
            e.1.push(*u);
        }
    }
    Ok(by.into_iter().filter(|(_, v)| v.0.len() >= 2).map(|(o, (a, b))| (o, a.len(), mean_sd(&a).0, mean_sd(&b).0)).collect())
}

/// Does the training ranking of variants predict the unseen half? (training gain, unseen gain) per variant in, out:
/// (variants, Spearman rho, its standard error ~ 1/sqrt(n-1), mean unseen gain of the best quarter by training gain, mean unseen gain of all).
/// A rho near zero (or negative) says picking winners from the training half has no value: the winners are selection luck.
pub fn persistence_of(pairs: &[(f64, f64)]) -> Option<(usize, f64, f64, f64, f64)> {
    let n = pairs.len();
    if n < 8 { return None; }
    let (a, b): (Vec<f64>, Vec<f64>) = pairs.iter().copied().unzip();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|i, j| a[*j].total_cmp(&a[*i]));
    let top: Vec<f64> = order.iter().take((n / 4).max(1)).map(|i| b[*i]).collect();
    Some((n, crate::stats::spearman(&a, &b), 1.0 / ((n - 1) as f64).sqrt(), mean_sd(&top).0, mean_sd(&b).0))
}

/// The persistence line for the human review: training-half rank vs unseen-half gain over every variant with both halves.
pub fn persistence_line(mem: &Connection) -> Result<String> {
    let tr = paired_all(mem, "5h", Half::Training)?;
    let un: BTreeMap<String, f64> = paired_all(mem, "5h", Half::Unseen)?.into_iter().map(|p| (p.tokens, p.mean_diff)).collect();
    let pairs: Vec<(f64, f64)> = tr.iter().filter_map(|p| un.get(&p.tokens).map(|u| (p.mean_diff, *u))).collect();
    Ok(match persistence_of(&pairs) {
        None => "  Rank persistence: too few variants with both halves".into(),
        Some((n, rho, se, top, all)) => format!("  Rank persistence (training gain -> unseen gain, {n} variants): Spearman rho {rho:+.2} (se {se:.2}); best quarter by training gain averages {top:+.2}/block unseen vs {all:+.2} for all variants. {}",
            if rho < 2.0 * se { "The training ranking does not significantly predict the unseen half: a training winner is not evidence by itself." } else { "The training ranking predicts the unseen half." }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: a ranking that carries over shows a high rho; an inverted one a negative rho and a weak top quarter; too few variants gives none.
    #[test]
    fn persistence_tells_carried_over_from_inverted_rankings() {
        let same: Vec<(f64, f64)> = (0..12).map(|i| (i as f64, 0.5 * i as f64)).collect();
        let (n, rho, se, top, all) = persistence_of(&same).unwrap();
        assert!(n == 12 && rho > 0.99 && top > all && rho > 2.0 * se);
        let inv: Vec<(f64, f64)> = (0..12).map(|i| (i as f64, -(i as f64))).collect();
        let (_, rho, _, top, all) = persistence_of(&inv).unwrap();
        assert!(rho < -0.99 && top < all, "winners of the training half are the unseen half's losers");
        assert!(persistence_of(&same[..5]).is_none());
    }

    /// Test: a variant that is better in every block gets a high t and survives; noise does not; the training view never uses
    /// the second half of the blocks.
    #[test]
    fn paired_review_separates_edge_from_noise_and_halves() {
        let m = Connection::open_in_memory().unwrap();
        crate::strategy::init(&m).unwrap();
        let ins = |t: &str, i: i64, r: f64| m.execute("INSERT INTO strategy_blocks(interval, tokens, equity, cut, idx, start, ret, trades, wins, ts, chain) VALUES ('5h', ?1, 5000, 'A', ?2, ?3, ?4, 10, 5, 1, 0)",
            rusqlite::params![t, i, i * 86_400, r]).unwrap();
        for i in 0..20 {
            let base = if i % 2 == 0 { 1.0 } else { -1.0 } * (1.0 + (i % 3) as f64);
            ins("baseline", i, base);
            ins("edge", i, base + 1.0 + 0.1 * (i % 2) as f64);                     // better in every block
            ins("noise", i, base + if i % 3 == 0 { 2.0 } else { -1.0 });          // sometimes better, sometimes worse
            ins("late", i, base + if i >= 10 { 5.0 } else { 0.0 });               // only better in the second half
        }
        let un = paired_all(&m, "5h", Half::Unseen).unwrap();
        let edge = un.iter().find(|p| p.tokens == "edge").unwrap();
        assert!(edge.t > 5.0 && edge.better_share == 1.0 && edge.ci.0 > 0.0, "{edge:?}");
        let noise = un.iter().find(|p| p.tokens == "noise").unwrap();
        assert!(noise.t < edge.t && noise.q > edge.q);
        let tr = paired_all(&m, "5h", Half::Training).unwrap();
        assert!(tr.iter().find(|p| p.tokens == "late").unwrap().mean_diff.abs() < 1e-9, "the training view never sees the second half");
        let (text, js) = review(&m, Half::Unseen).unwrap();
        assert!(js["survivors"].as_array().unwrap().iter().any(|x| x == "edge"), "the consistent edge survives: {text}");
        assert!(!js["survivors"].as_array().unwrap().iter().any(|x| x == "noise"), "noise does not: {text}");
        assert!((p_one_sided(0.0) - 0.5).abs() < 1e-6 && p_one_sided(3.0) < 0.002);
    }
}
