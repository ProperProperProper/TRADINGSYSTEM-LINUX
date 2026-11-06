//! The lab's own small model (owner 2026-10-13: "it must be narrow and specific to this bot, trading and learning ... and it can use machine
//! learning too"). A Gaussian-process surrogate over OPTION SETS: it learns from every variant the lab has already tested which option
//! combinations tend to have a high training-half edge, says how sure it is, and proposes the untested valid combination with the best
//! optimistic estimate (upper confidence bound) -- standard Bayesian optimisation over a small discrete space.
//!
//! Why a Gaussian process and not a neural network: the lab has only a few hundred tested variants (~240 at 5h). A network would memorise
//! them; a GP with a combinatorial kernel is the right size, gives an honest uncertainty, and trains in milliseconds on the CPU.
//!
//! Hard rules kept: it learns from the TRAINING half only (`h1`), never the holdout the lab uses to judge variants; its requests go through
//! the normal validated queue, which still tests them on both cuts; its `why` text contains no numbers it was not shown.
//!
//! ROLES (what the Hermes agents did, done by code + this model; see docs/BRAIN.md for the parity table): `cycle` is the strategist's job -- pick
//! untested variants and queue them through the same validated queue (`lab_queue_test`, author `brain-strategist`), so the lab's tests, scoreboard and
//! checks treat them exactly like Hermes requests. Three policies take turns, least-used first, and every proposal is logged with its policy: an
//! optimistic-bonus rule (UCB), the additive average (the one model that learns), and a RANDOM control. `report` compares them (and Hermes) on real outcomes, so the model is only
//! trusted if it beats chance.
//!
//! Kernel: k(A,B) = a*|A∩B| + b*|fam(A)∩fam(B)| + c*[A==B and |A|==2] + d     (options shared, exclusive families shared, identical pair, bias)

use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeSet;

/// One tested variant: its options and the training-half gain per block (points vs the current strategy's blocks, as the lab measured).
#[derive(Debug, Clone)]
pub struct Obs { pub options: BTreeSet<String>, pub y: f64 }

#[derive(Debug, Clone, Copy)]
pub struct Hyper { pub a: f64, pub b: f64, pub c: f64, pub d: f64, pub noise: f64 }
impl Default for Hyper { fn default() -> Self { Hyper { a: 0.4, b: 0.2, c: 0.2, d: 0.3, noise: 0.4 } } }

fn family(t: &str) -> Option<usize> { crate::strategy::GROUPS.iter().position(|g| g.contains(&t)) }
fn fams(s: &BTreeSet<String>) -> BTreeSet<usize> { s.iter().filter_map(|t| family(t)).collect() }

pub fn kernel(h: &Hyper, x: &BTreeSet<String>, z: &BTreeSet<String>) -> f64 {
    let shared = x.intersection(z).count() as f64;
    let fshared = fams(x).intersection(&fams(z)).count() as f64;
    let same_pair = if x == z && x.len() == 2 { 1.0 } else { 0.0 };
    h.a * shared + h.b * fshared + h.c * same_pair + h.d
}

/// Lower-triangular Cholesky factor of a symmetric positive-definite matrix (row-major n x n); None if it is not positive definite.
pub fn cholesky(a: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut l = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut s = a[i * n + j];
            for k in 0..j { s -= l[i * n + k] * l[j * n + k]; }
            if i == j { if s <= 1e-12 { return None; } l[i * n + i] = s.sqrt(); } else { l[i * n + j] = s / l[j * n + j]; }
        }
    }
    Some(l)
}
fn solve_lower(l: &[f64], n: usize, b: &[f64]) -> Vec<f64> {
    let mut x = vec![0.0; n];
    for i in 0..n { let mut s = b[i]; for k in 0..i { s -= l[i * n + k] * x[k]; } x[i] = s / l[i * n + i]; }
    x
}
fn solve_upper_t(l: &[f64], n: usize, b: &[f64]) -> Vec<f64> {
    let mut x = vec![0.0; n];
    for i in (0..n).rev() { let mut s = b[i]; for k in i + 1..n { s -= l[k * n + i] * x[k]; } x[i] = s / l[i * n + i]; }
    x
}

/// A fitted surrogate (targets are standardised internally).
pub struct Model { h: Hyper, xs: Vec<BTreeSet<String>>, l: Vec<f64>, alpha: Vec<f64>, mean: f64, sd: f64, pub log_ml: f64 }

impl Model {
    pub fn fit(obs: &[Obs], h: Hyper) -> Option<Model> {
        let n = obs.len();
        if n < 3 { return None; }
        let mean = obs.iter().map(|o| o.y).sum::<f64>() / n as f64;
        let sd = (obs.iter().map(|o| (o.y - mean).powi(2)).sum::<f64>() / n as f64).sqrt().max(1e-6);
        let ys: Vec<f64> = obs.iter().map(|o| (o.y - mean) / sd).collect();
        let xs: Vec<BTreeSet<String>> = obs.iter().map(|o| o.options.clone()).collect();
        let mut k = vec![0.0; n * n];
        for i in 0..n { for j in 0..n { k[i * n + j] = kernel(&h, &xs[i], &xs[j]) + if i == j { h.noise } else { 0.0 }; } }
        let l = cholesky(&k, n)?;
        let alpha = solve_upper_t(&l, n, &solve_lower(&l, n, &ys));
        let log_ml = -0.5 * ys.iter().zip(&alpha).map(|(y, a)| y * a).sum::<f64>() - (0..n).map(|i| l[i * n + i].ln()).sum::<f64>() - 0.5 * n as f64 * (2.0 * std::f64::consts::PI).ln();
        Some(Model { h, xs, l, alpha, mean, sd, log_ml })
    }

    /// (predicted gain, standard deviation) for a set of options, in the original units.
    pub fn predict(&self, x: &BTreeSet<String>) -> (f64, f64) {
        let n = self.xs.len();
        let ks: Vec<f64> = self.xs.iter().map(|z| kernel(&self.h, x, z)).collect();
        let mu = ks.iter().zip(&self.alpha).map(|(k, a)| k * a).sum::<f64>();
        let v = solve_lower(&self.l, n, &ks);
        let var = (kernel(&self.h, x, x) - v.iter().map(|t| t * t).sum::<f64>()).max(1e-9);
        (self.mean + self.sd * mu, self.sd * var.sqrt())
    }
}

/// Picks hyper-parameters by coordinate search on the log marginal likelihood (training data only).
pub fn tune(obs: &[Obs]) -> Hyper {
    let mut h = Hyper::default();
    let grid = [0.02, 0.05, 0.1, 0.2, 0.4, 0.8, 1.6];
    let score = |h: &Hyper| Model::fit(obs, *h).map_or(f64::MIN, |m| m.log_ml);
    let mut best = score(&h);
    for _ in 0..2 {
        for field in 0..5 {
            for g in grid {
                let mut t = h;
                match field { 0 => t.a = g, 1 => t.b = g, 2 => t.c = g, 3 => t.d = g, _ => t.noise = g.max(0.05) }
                let s = score(&t);
                if s > best { best = s; h = t; }
            }
        }
    }
    h
}

fn ranks(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].total_cmp(&v[b]));
    let mut r = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] { j += 1; }
        for &k in &idx[i..=j] { r[k] = (i + j) as f64 / 2.0; }
        i = j + 1;
    }
    r
}
/// Spearman rank correlation (average ranks for ties); 0 for fewer than 3 points or no spread.
pub fn spearman(a: &[f64], b: &[f64]) -> f64 {
    if a.len() < 3 { return 0.0; }
    let (ra, rb) = (ranks(a), ranks(b));
    let n = a.len() as f64;
    let (ma, mb) = (ra.iter().sum::<f64>() / n, rb.iter().sum::<f64>() / n);
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for i in 0..a.len() { cov += (ra[i] - ma) * (rb[i] - mb); va += (ra[i] - ma).powi(2); vb += (rb[i] - mb).powi(2); }
    if va == 0.0 || vb == 0.0 { 0.0 } else { cov / (va.sqrt() * vb.sqrt()) }
}

/// The baseline the surrogate must beat: the plain additive rule "average training gain of the earlier variants that used each option".
pub fn additive_predict(train: &[Obs], x: &BTreeSet<String>) -> f64 {
    let overall = train.iter().map(|o| o.y).sum::<f64>() / train.len().max(1) as f64;
    let per: Vec<f64> = x.iter().map(|t| {
        let ys: Vec<f64> = train.iter().filter(|o| o.options.contains(t)).map(|o| o.y).collect();
        if ys.is_empty() { overall } else { ys.iter().sum::<f64>() / ys.len() as f64 }
    }).collect();
    per.iter().sum::<f64>() / per.len().max(1) as f64
}

#[derive(Debug, Clone)]
pub struct WalkForward { pub tested: usize, pub rho_gp: f64, pub rho_additive: f64, pub top_gp: f64, pub top_additive: f64, pub base: f64 }

/// Walk-forward test in time order: fit on the first `t` variants, predict the next `step`, slide. Reports rank correlation and the mean actual
/// gain of the top quarter by prediction, for the surrogate and for the additive baseline, against the plain average (no skill).
pub fn walk_forward(obs: &[Obs], start: usize, step: usize) -> WalkForward {
    let (mut pg, mut pa, mut act) = (vec![], vec![], vec![]);
    let mut t = start;
    while t + 1 < obs.len() {
        let end = (t + step).min(obs.len());
        let train = &obs[..t];
        if let Some(m) = Model::fit(train, tune(train)) {
            for o in &obs[t..end] { pg.push(m.predict(&o.options).0); pa.push(additive_predict(train, &o.options)); act.push(o.y); }
        }
        t = end;
    }
    let top = |p: &[f64]| { let mut i: Vec<usize> = (0..p.len()).collect(); i.sort_by(|&a, &b| p[b].total_cmp(&p[a])); let k = (p.len() / 4).max(1); i[..k].iter().map(|&j| act[j]).sum::<f64>() / k as f64 };
    WalkForward { tested: act.len(), rho_gp: spearman(&pg, &act), rho_additive: spearman(&pa, &act), top_gp: if act.is_empty() { 0.0 } else { top(&pg) }, top_additive: if act.is_empty() { 0.0 } else { top(&pa) },
        base: act.iter().sum::<f64>() / act.len().max(1) as f64 }
}

/// Every 5h variant at cut A, oldest first, with its training-half gain per block (`h1`). Only the regime the live strategy currently runs.
pub fn load(mem: &Connection) -> Result<Vec<Obs>> {
    crate::strategy::init(mem)?;
    let regime = crate::strategy::current_regime(mem);
    let mut st = mem.prepare("SELECT tokens, h1 FROM strategy_runs WHERE regime = ?1 AND interval = '5h' AND cut = 'A' AND tokens != 'baseline' ORDER BY ts, id")?;
    let rows = st.query_map([regime], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().map(|(t, y)| Obs { options: t.split('+').map(String::from).collect(), y }).collect())
}

/// Every valid 1- and 2-option combination from the vocabulary.
pub fn candidates() -> Vec<BTreeSet<String>> {
    let toks: Vec<String> = crate::strategy::VOCAB.iter().map(|(t, _)| t.to_string()).collect();
    let mut out = vec![];
    for (i, a) in toks.iter().enumerate() {
        let one = vec![a.clone()];
        if crate::strategy::why_invalid(&one).is_none() { out.push(one.into_iter().collect()); }
        for b in &toks[i + 1..] {
            let two = vec![a.clone(), b.clone()];
            if crate::strategy::why_invalid(&two).is_none() { out.push(two.into_iter().collect()); }
        }
    }
    out
}

/// The `n` untested valid combinations with the best upper confidence bound (mean + kappa*sd), at most one per leading option so the batch is varied.
pub fn propose(obs: &[Obs], tested: &BTreeSet<String>, n: usize, kappa: f64) -> Vec<(BTreeSet<String>, f64, f64)> {
    let Some(m) = Model::fit(obs, tune(obs)) else { return vec![] };
    let mut scored: Vec<(BTreeSet<String>, f64, f64)> = candidates().into_iter()
        .filter(|c| !tested.contains(&crate::strategy::key(&c.iter().cloned().collect::<Vec<_>>())))
        .map(|c| { let (mu, sd) = m.predict(&c); (c, mu, sd) }).collect();
    scored.sort_by(|a, b| (b.1 + kappa * b.2).total_cmp(&(a.1 + kappa * a.2)));
    let (mut out, mut used) = (vec![], BTreeSet::new());
    for s in scored {
        let lead = s.0.iter().next().cloned().unwrap_or_default();
        if used.insert(lead) { out.push(s); }
        if out.len() >= n { break; }
    }
    out
}

/// Human-readable walk-forward report on the lab's real history (training-half targets only).
pub fn report_eval(obs: &[Obs]) -> String {
    let mut out = vec![format!("{} tested 5h variants (training-half gain per block), oldest first; walk-forward = fit on the earlier ones, predict the next", obs.len())];
    if obs.len() < 30 { out.push("too few variants to evaluate".into()); return out.join("\n"); }
    out.push("start  predicted  rank-corr(GP)  rank-corr(additive)  top-quarter actual gain: GP | additive | plain average".into());
    for start in [30usize, 60, 100, 140] {
        if start + 10 > obs.len() { continue; }
        let w = walk_forward(obs, start, 5);
        out.push(format!("{start:>5}  {:>9}  {:>13.3}  {:>19.3}  {:>+8.3} | {:>+8.3} | {:>+8.3}", w.tested, w.rho_gp, w.rho_additive, w.top_gp, w.top_additive, w.base));
    }
    out.join("\n")
}

/// Author prefix of the code-driven agents.
pub const PREFIX: &str = "brain-";
pub const STRATEGIST: &str = "brain-strategist";
pub const POLICIES: [&str; 4] = ["heuristic", "ucb", "additive", "random"];

pub fn init_tables(mem: &Connection) -> Result<()> {
    mem.execute_batch("CREATE TABLE IF NOT EXISTS brain_proposals (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, policy TEXT NOT NULL, author TEXT NOT NULL, interval TEXT NOT NULL, tokens TEXT NOT NULL, predicted REAL, spread REAL, queued INTEGER NOT NULL DEFAULT 0, note TEXT);")?;
    Ok(())
}

/// Optimistic-bonus rule: the additive average of past training gains plus a bonus that is larger for options tested few times.
pub fn ucb_score(train: &[Obs], x: &BTreeSet<String>, kappa: f64) -> (f64, f64) {
    let mean = additive_predict(train, x);
    let m = train.iter().map(|o| o.y).sum::<f64>() / train.len().max(1) as f64;
    let sd = (train.iter().map(|o| (o.y - m).powi(2)).sum::<f64>() / train.len().max(1) as f64).sqrt();
    let bonus: f64 = x.iter().map(|t| 1.0 / (1.0 + train.iter().filter(|o| o.options.contains(t)).count() as f64).sqrt()).sum::<f64>() / x.len().max(1) as f64;
    (mean, kappa * sd * bonus)
}

fn xorshift(state: &mut u64) -> u64 { *state ^= *state << 13; *state ^= *state >> 7; *state ^= *state << 17; *state }

/// The next `n` variants under one policy, never one in `avoid` (tested or already queued); at most one per leading option so a batch is varied.
pub fn pick(policy: &str, obs: &[Obs], avoid: &BTreeSet<String>, n: usize, seed: u64) -> Vec<(BTreeSet<String>, f64, f64)> {
    let free = |c: &BTreeSet<String>| !avoid.contains(&crate::strategy::key(&c.iter().cloned().collect::<Vec<_>>()));
    let mut scored: Vec<(BTreeSet<String>, f64, f64)> = match policy {
        // the average of each option's past training gains: the one model whose error falls and whose ranking improves as the lab tests more variants
        // (learning curve, docs/BRAIN.md); the Gaussian process did not learn and was dropped from the rotation
        "additive" => { let mut v: Vec<_> = candidates().into_iter().filter(|c| free(c)).map(|c| { let m = additive_predict(obs, &c); (c, m, 0.0) }).collect(); v.sort_by(|a, b| b.1.total_cmp(&a.1)); v }
        "random" => {
            let mut v: Vec<_> = candidates().into_iter().filter(|c| free(c)).map(|c| (c, 0.0, 0.0)).collect();
            let mut st = seed | 1;
            for i in (1..v.len()).rev() { let j = (xorshift(&mut st) % (i as u64 + 1)) as usize; v.swap(i, j); }
            v
        }
        _ => { let mut v: Vec<_> = candidates().into_iter().filter(|c| free(c)).map(|c| { let (mu, b) = ucb_score(obs, &c, 1.0); (c, mu, b) }).collect(); v.sort_by(|a, b| (b.1 + b.2).total_cmp(&(a.1 + a.2))); v }
    };
    let (mut out, mut used) = (vec![], BTreeSet::new());
    for s in scored.drain(..) {
        if used.insert(s.0.iter().next().cloned().unwrap_or_default()) { out.push(s); }
        if out.len() >= n { break; }
    }
    out
}

/// The lab's own ranked list of untested variants (`lab_untested`, the list the Hermes strategist picks from), top first, one per leading option.
pub fn pick_heuristic(mem: &Connection, avoid: &BTreeSet<String>, n: usize) -> Vec<(BTreeSet<String>, f64, f64)> {
    let list = crate::labtools::untested(mem, "5h", 60).unwrap_or_default();
    let (mut out, mut used) = (vec![], BTreeSet::new());
    for k in list {
        let toks: Vec<String> = k.split('+').map(String::from).collect();
        if avoid.contains(&crate::strategy::key(&toks)) || crate::strategy::why_invalid(&toks).is_some() { continue; }
        let set: BTreeSet<String> = toks.into_iter().collect();
        if used.insert(set.iter().next().cloned().unwrap_or_default()) { out.push((set, 0.0, 0.0)); }
        if out.len() >= n { break; }
    }
    out
}

/// Policy `build`: untested valid combinations that contain an option from a variant which
/// survives the paired, multiple-testing-adjusted TRAINING-half screen, best average past
/// training gain first, one per leading option. It never reads holdout-derived statuses.
pub fn pick_build(mem: &Connection, obs: &[Obs], avoid: &BTreeSet<String>, n: usize) -> Vec<(BTreeSet<String>, f64, f64)> {
    let winners: BTreeSet<String> = crate::quant::training_survivor_options(mem).unwrap_or_default().into_iter().collect();
    let mut v: Vec<_> = candidates().into_iter()
        .filter(|c| c.iter().any(|o| winners.contains(o)) && !avoid.contains(&crate::strategy::key(&c.iter().cloned().collect::<Vec<_>>())))
        .map(|c| { let m = additive_predict(obs, &c); (c, m, 0.0) }).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (mut out, mut used) = (vec![], BTreeSet::new());
    for s in v { if used.insert(s.0.iter().find(|o| !winners.contains(*o)).or(s.0.iter().next()).cloned().unwrap_or_default()) { out.push(s); }
        if out.len() >= n { break; }
    }
    out
}

/// The policy that has proposed least so far (ties in `POLICIES` order), so the three arms stay balanced.
pub fn next_policy(mem: &Connection) -> Result<&'static str> {
    init_tables(mem)?;
    let mut best = (f64::MAX, "build");
    for (p, w) in WEIGHTED {
        let n: i64 = mem.query_row("SELECT count(*) FROM brain_proposals WHERE policy = ?1 AND queued = 1", [p], |r| r.get(0))?;
        if (n as f64 / w) < best.0 { best = (n as f64 / w, p); }
    }
    Ok(best.1)
}

/// Turn-taking weights: `build` (the exploit policy that builds on options that already passed) gets three turns in seven; the other four arms (including the random control) one each.
const WEIGHTED: [(&str, f64); 5] = [("build", 3.0), ("heuristic", 1.0), ("ucb", 1.0), ("additive", 1.0), ("random", 1.0)];

fn why_text(policy: &str) -> &'static str {
    match policy {
        "build" => "policy build: untested valid combination containing an option from a paired, multiple-testing-adjusted training-half survivor; best average past training gain first",
        "heuristic" => "policy heuristic: top of the lab's own ranked list of untested variants (no language model)",
        "ucb" => "policy ucb: untested valid combination with the best optimistic training-half estimate; options tested few times get a bonus",
        "additive" => "policy additive: untested valid combination with the best average past training gain of its options (the model that improves as the lab tests more variants)",
        _ => "policy random: untested valid combination drawn at random, the control arm that shows whether the model beats chance",
    }
}

/// The strategist's job: queue up to `n` untested variants under the next policy. Returns one line per request.
pub fn cycle(ctx: &crate::context::Ctx, n: usize) -> Result<Vec<String>> {
    init_tables(ctx.mem)?;
    // the queue allows 4 open requests per author (labtools.rs): at the cap, wait for results instead of proposing and being refused
    let open: i64 = ctx.mem.query_row("SELECT count(*) FROM test_queue WHERE author = ?1 AND status IN ('queued', 'testing') AND ts > ?2", rusqlite::params![STRATEGIST, crate::util::now_ts() - 2 * 86_400], |r| r.get(0))?;
    if open >= 4 { return Ok(vec![format!("waiting: {open} open requests already, none proposed until their results are in")]); }
    let obs = load(ctx.mem)?;
    let mut avoid: BTreeSet<String> = obs.iter().map(|o| crate::strategy::key(&o.options.iter().cloned().collect::<Vec<_>>())).collect();
    if let Ok(mut st) = ctx.mem.prepare("SELECT tokens FROM test_queue WHERE interval = '5h' AND status IN ('queued', 'testing')") {
        if let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) { avoid.extend(rows.flatten()); }
    }
    let policy = next_policy(ctx.mem)?;
    let now = crate::util::now_ts();
    let mut lines = vec![];
    let mut policy = policy;
    let mut picks = match policy { "heuristic" => pick_heuristic(ctx.mem, &avoid, n), "build" => pick_build(ctx.mem, &obs, &avoid, n), p => pick(p, &obs, &avoid, n, now as u64) };
    if picks.is_empty() && policy == "build" { policy = "additive"; picks = pick(policy, &obs, &avoid, n, now as u64); } // nothing validated yet: say which policy really chose
    for (c, mu, sd) in picks {
        let opts: Vec<String> = c.iter().cloned().collect();
        let key = crate::strategy::key(&opts);
        let r = crate::labtools::call(ctx, "lab_queue_test", &serde_json::json!({"interval": "5h", "options": opts, "why": why_text(policy), "author": STRATEGIST}));
        let (queued, note) = match &r { Ok(t) => (t.starts_with("queued"), t.lines().next().unwrap_or("").to_string()), Err(e) => (false, format!("refused: {e:#}")) };
        ctx.mem.execute("INSERT INTO brain_proposals(ts, policy, author, interval, tokens, predicted, spread, queued, note) VALUES (?1,?2,?3,'5h',?4,?5,?6,?7,?8)", rusqlite::params![now, policy, STRATEGIST, key, mu, sd, queued as i64, note])?;
        lines.push(format!("{policy}: 5h|{key} -> {note}"));
        if !queued && note.contains("open requests") { break; }
    }
    Ok(lines)
}

/// Head-to-head table on real outcomes: per brain policy, how many requests were tested and how many passed the lab's verdict.
pub fn report(mem: &Connection) -> Result<String> {
    init_tables(mem)?;
    let lb = crate::strategy::leaderboard(mem)?;
    let status = |key: &str| lb.iter().find(|r| r["interval"] == "5h" && r["tokens"] == key).map(|r| (r["status"].as_str().unwrap_or("").to_string(), r["a"]["h1"].as_f64().unwrap_or(0.0)));
    let mut rows: Vec<(String, Vec<String>)> = vec![];
    let mut policies: Vec<String> = std::iter::once("build").chain(POLICIES.iter().copied()).map(String::from).collect();
    { let mut st = mem.prepare("SELECT DISTINCT policy FROM brain_proposals ORDER BY policy")?; for p in st.query_map([], |r| r.get::<_, String>(0))?.flatten() { if !policies.contains(&p) { policies.push(p); } } }
    for p in &policies { let mut st = mem.prepare("SELECT tokens FROM brain_proposals WHERE policy = ?1 AND queued = 1")?; rows.push((format!("brain/{p}"), st.query_map([p.as_str()], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?)); }
    let all: Vec<(String, f64)> = lb.iter().filter(|r| r["interval"] == "5h").map(|r| (r["status"].as_str().unwrap_or("").to_string(), r["a"]["h1"].as_f64().unwrap_or(0.0))).collect();
    let mut out = vec!["source                 requested  tested  passed-holdout  validated+candidate  mean training gain".to_string()];
    let mut line = |name: &str, requested: usize, tested: &[(String, f64)]| {
        let pass = tested.iter().filter(|t| t.0 != "failed-holdout").count();
        let vc = tested.iter().filter(|t| t.0 == "validated" || t.0 == "candidate").count();
        let gain = if tested.is_empty() { "-".to_string() } else { format!("{:+.2}", tested.iter().map(|t| t.1).sum::<f64>() / tested.len() as f64) };
        out.push(format!("{name:<22} {requested:>9}  {:>6}  {:>14}  {:>19}  {gain:>18}", tested.len(), pass, vc));
    };
    for (name, keys) in &rows { let tested: Vec<(String, f64)> = keys.iter().filter_map(|k| status(k)).collect(); line(name, keys.len(), &tested); }
    line("ALL tested variants", all.len(), &all);
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[&str]) -> BTreeSet<String> { v.iter().map(|s| s.to_string()).collect() }
    fn obs(v: &[(&[&str], f64)]) -> Vec<Obs> { v.iter().map(|(o, y)| Obs { options: set(o), y: *y }).collect() }

    #[test]
    fn cholesky_solves_a_known_system() {
        let a = [4.0, 2.0, 2.0, 3.0];
        let l = cholesky(&a, 2).unwrap();
        let x = solve_upper_t(&l, 2, &solve_lower(&l, 2, &[2.0, 1.0]));
        assert!((4.0 * x[0] + 2.0 * x[1] - 2.0).abs() < 1e-9 && (2.0 * x[0] + 3.0 * x[1] - 1.0).abs() < 1e-9);
        assert!(cholesky(&[1.0, 2.0, 2.0, 1.0], 2).is_none(), "not positive definite");
    }

    #[test]
    fn spearman_known_values() {
        assert!((spearman(&[1.0, 2.0, 3.0, 4.0], &[10.0, 20.0, 30.0, 40.0]) - 1.0).abs() < 1e-12);
        assert!((spearman(&[1.0, 2.0, 3.0, 4.0], &[4.0, 3.0, 2.0, 1.0]) + 1.0).abs() < 1e-12);
        assert_eq!(spearman(&[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]), 0.0);
    }

    #[test]
    fn the_surrogate_reproduces_its_training_data_and_is_unsure_far_away() {
        let data = obs(&[(&["vwap"], 1.0), (&["cmf"], -1.0), (&["vwap", "obv"], 1.5), (&["cmf", "obv"], -0.5), (&["obv"], 0.5), (&["vwap", "cmf"], 0.0), (&["entry0"], 0.2), (&["flatenter"], -0.2)]);
        let m = Model::fit(&data, Hyper { noise: 0.05, ..Hyper::default() }).unwrap();
        for o in &data { assert!((m.predict(&o.options).0 - o.y).abs() < 0.35, "{:?}: {:?}", o.options, m.predict(&o.options)); }
        let (_, sd_known) = m.predict(&set(&["vwap"]));
        let (_, sd_new) = m.predict(&set(&["entrywide", "univ60"]));
        assert!(sd_new > sd_known, "an unseen combination must be less certain ({sd_new} vs {sd_known})");
    }

    #[test]
    fn it_learns_an_additive_effect_and_beats_the_plain_average_walk_forward() {
        // synthetic truth: y = sum of option effects + deterministic noise; the surrogate must rank later variants better than chance
        let effect = |t: &str| match t { "vwap" => 1.0, "cmf" => -1.0, "obv" => 0.6, "entry0" => 0.2, "flatenter" => -0.4, "entrywide" => -0.2, _ => 0.0 };
        let toks = ["vwap", "cmf", "obv", "entry0", "flatenter", "entrywide"];
        let mut v = vec![]; let mut s = 7u64;
        for i in 0..toks.len() { for j in i..toks.len() {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let noise = ((s >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 0.3;
            let o = if i == j { vec![toks[i]] } else { vec![toks[i], toks[j]] };
            v.push((o.iter().map(|t| effect(t)).sum::<f64>() + noise, o));
        } }
        let data: Vec<Obs> = v.iter().map(|(y, o)| Obs { options: set(o), y: *y }).collect();
        // shuffle deterministically so early and late variants mix
        let mut data = data; let mut s2 = 99u64; for i in (1..data.len()).rev() { s2 = s2.wrapping_mul(6364136223846793005).wrapping_add(1); data.swap(i, (s2 >> 33) as usize % (i + 1)); }
        let w = walk_forward(&data, 10, 3);
        assert!(w.rho_gp > 0.5, "rank correlation {w:?}");
        assert!(w.top_gp > w.base, "top quarter must beat the plain average {w:?}");
    }

    #[test]
    fn every_policy_proposes_valid_untested_varied_variants_and_random_is_seeded() {
        let data = obs(&[(&["vwap"], 1.0), (&["cmf"], -1.0), (&["obv"], 0.5), (&["entry0"], 0.2), (&["vwap", "obv"], 1.4), (&["flatenter"], 0.1)]);
        let avoid: BTreeSet<String> = ["vwap", "cmf", "obv", "entry0", "flatenter", "obv+vwap"].iter().map(|s| s.to_string()).collect();
        for policy in POLICIES {
            let p = pick(policy, &data, &avoid, 3, 42);
            assert_eq!(p.len(), 3, "{policy}");
            for (c, _, _) in &p { let v: Vec<String> = c.iter().cloned().collect(); assert!(crate::strategy::why_invalid(&v).is_none() && !avoid.contains(&crate::strategy::key(&v)), "{policy} {v:?}"); }
        }
        assert_eq!(pick("random", &data, &avoid, 3, 42).iter().map(|x| x.0.clone()).collect::<Vec<_>>(), pick("random", &data, &avoid, 3, 42).iter().map(|x| x.0.clone()).collect::<Vec<_>>());
        assert_ne!(pick("random", &data, &avoid, 3, 1).iter().map(|x| x.0.clone()).collect::<Vec<_>>(), pick("random", &data, &avoid, 3, 2).iter().map(|x| x.0.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn the_bonus_favours_options_tested_few_times() {
        let data = obs(&[(&["vwap"], 1.0), (&["vwap", "obv"], 2.0), (&["vwap", "cmf"], 0.0), (&["vwap", "entry0"], 1.0), (&["flatenter"], 1.0)]);
        let (_, b_common) = ucb_score(&data, &set(&["vwap"]), 1.0);
        let (_, b_rare) = ucb_score(&data, &set(&["flatenter"]), 1.0);
        assert!(b_rare > b_common, "the rarely tested option gets the larger bonus ({b_rare} vs {b_common})");
        assert!(ucb_score(&data, &set(&["flatenter"]), 2.0).1 > b_rare, "kappa scales the bonus");
    }

    #[test]
    fn a_cycle_queues_through_the_lab_queue_and_logs_each_policy() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::Config::for_root(dir.path(), "synthetic");
        let mem = Connection::open_in_memory().unwrap();
        let data = Connection::open_in_memory().unwrap();
        crate::strategy::init(&mem).unwrap();
        crate::labtools::init(&mem).unwrap();
        // a few tested variants so the models have something to learn from
        for (i, (t, h1)) in [("vwap", 1.0), ("cmf", -1.0), ("obv", 0.5), ("entry0", 0.2), ("flatenter", 0.1), ("obv+vwap", 1.4)].iter().enumerate() {
            mem.execute("INSERT INTO strategy_runs(ts, tokens, interval, equity, cut, blocks, spy_bh, mean, median, pos, worst, h1, h2, trades, regime) VALUES (?1,?2,'5h',5000,'A',20,1,?3,?3,50,-3,?3,?3,5,?4)",
                rusqlite::params![i as i64, t, h1, crate::strategy::current_regime(&mem)]).unwrap();
        }
        let ctx = crate::context::Ctx { cfg: &cfg, mem: &mem, data: &data };
        let mut policies = vec![];
        for _ in 0..4 {
            let lines = cycle(&ctx, 1).unwrap();
            assert_eq!(lines.len(), 1, "{lines:?}");
            policies.push(lines[0].split(':').next().unwrap().to_string());
        }
        assert!(policies.iter().all(|p| ["build", "heuristic", "ucb", "additive", "random"].contains(&p.as_str())), "{policies:?}");
        let queued: i64 = mem.query_row("SELECT count(*) FROM test_queue WHERE author = 'brain-strategist'", [], |r| r.get(0)).unwrap();
        let logged: i64 = mem.query_row("SELECT count(*) FROM brain_proposals WHERE queued = 1", [], |r| r.get(0)).unwrap();
        assert_eq!((queued, logged), (4, 4));
        let refused = cycle(&ctx, 2).unwrap();
        assert!(refused[0].contains("open requests"), "the per-author limit of 4 open requests stops a fifth: {refused:?}");
        let logged_after: i64 = mem.query_row("SELECT count(*) FROM brain_proposals WHERE queued = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(logged_after, 4);
        let rep = report(&mem).unwrap();
        assert!(rep.contains("brain/build") && rep.contains("brain/heuristic") && rep.contains("brain/ucb") && rep.contains("brain/random") && rep.contains("ALL tested variants"), "{rep}");
    }

    #[test]
    fn build_uses_only_training_half_multiplicity_survivors() {
        let mem = Connection::open_in_memory().unwrap();
        crate::strategy::init(&mem).unwrap();
        let data = obs(&[(&["obv"], 1.0), (&["vwap"], -1.0)]);
        assert!(pick_build(&mem, &data, &BTreeSet::new(), 3).is_empty(), "no paired block evidence means no seed");
        let insert = |tokens: &str, i: i64, ret: f64| {
            mem.execute("INSERT INTO strategy_blocks(interval, tokens, equity, cut, idx, start, ret, trades, wins, ts, chain) VALUES ('5h', ?1, 5000, 'A', ?2, ?3, ?4, 10, 5, 1, 0)",
                rusqlite::params![tokens, i, i * 86_400, ret]).unwrap();
        };
        for i in 0..20 {
            let base = if i % 2 == 0 { 1.0 } else { -1.0 };
            insert("baseline", i, base);
            insert("obv", i, base + if i < 10 { 1.0 + 0.1 * (i % 2) as f64 } else { -3.0 });
            insert("vwap", i, base + if i < 10 { -1.0 - 0.1 * (i % 2) as f64 } else { 4.0 });
        }
        assert_eq!(crate::quant::training_survivor_options(&mem).unwrap(), vec!["obv"]);
        let picks = pick_build(&mem, &data, &set(&["obv", "vwap"]), 3);
        assert!(!picks.is_empty() && picks.iter().all(|p| p.0.contains("obv")), "the unseen-only vwap result must not seed build: {picks:?}");
    }

    #[test]
    fn the_policies_take_turns_least_used_first() {
        let m = Connection::open_in_memory().unwrap();
        init_tables(&m).unwrap();
        let mut seen = vec![];
        for i in 0..8 {
            let p = next_policy(&m).unwrap();
            seen.push(p);
            m.execute("INSERT INTO brain_proposals(ts, policy, author, interval, tokens, queued) VALUES (?1, ?2, 'brain-strategist', '5h', ?3, 1)", rusqlite::params![i, p, format!("k{i}")]).unwrap();
        }
        assert_eq!(seen, vec!["build", "heuristic", "ucb", "additive", "random", "build", "build", "build"], "build gets three turns in seven");
    }

    #[test]
    fn proposals_are_valid_untested_and_varied() {
        let data = obs(&[(&["vwap"], 1.0), (&["cmf"], -1.0), (&["obv"], 0.5), (&["entry0"], 0.2), (&["vwap", "obv"], 1.4)]);
        let tested: BTreeSet<String> = ["vwap", "cmf", "obv", "entry0", "obv+vwap"].iter().map(|s| s.to_string()).collect();
        let p = propose(&data, &tested, 4, 1.0);
        assert_eq!(p.len(), 4);
        let leads: BTreeSet<&String> = p.iter().filter_map(|x| x.0.iter().next()).collect();
        assert_eq!(leads.len(), 4, "one per leading option");
        for (c, _, _) in &p {
            let v: Vec<String> = c.iter().cloned().collect();
            assert!(crate::strategy::why_invalid(&v).is_none() && !tested.contains(&crate::strategy::key(&v)));
        }
    }
}
