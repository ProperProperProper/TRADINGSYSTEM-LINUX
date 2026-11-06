//! A3.3 improvement loop, harness side, judged by the owner's own standard (bot docs/STRATEGY.md): a variant of the CURRENT strategy that adds ENTRY options
//! (never signals, direction, exits or the universe) is kept only if, on UNSEEN walk-forward blocks, it beats the current strategy by at least 1.0 point per
//! 14 sessions (scaled to the block length), with a win rate that is not lower, and does so on at least 4 of 5 block offsets ("cuts"). At most 2 options are
//! added. The options are the bot's own vocabulary (`live_options::apply_token`), so a kept variant is exactly what the bot's tester could promote; `ml` (the
//! network's score read by the engine's ML gate) is research only and can never be promoted. A proposal is one token chosen from a fixed list; no code, no
//! numbers. Every fold chooses its setting only from candles before its test block (see `walkfwd`), real data only.
use anyhow::{Result, bail};
use moomoo_bot::engine::live_options;
use moomoo_bot::engine::scores;
use moomoo_bot::engine::walkforward::SearchSpace;
use moomoo_bot::engine::{Interval, START_CAPITAL};
use rusqlite::Connection;
use std::time::Instant;

use crate::learner;
use crate::market::Built;
use crate::walkfwd::{self, Rule};

/// The experiments.proposer value of this loop.
pub const PROPOSER: &str = "agent:improve";
/// Block offsets ("cuts") on the all-symbol window (20 scored sessions): the first training window is 8, 9, 10, 11 or 12 sessions, then blocks of `BLOCK` sessions.
pub const CUT_TRAIN: [usize; 5] = [8, 9, 10, 11, 12];
/// The same on the deep market (about 43 scored sessions): the bot's own 14-session search window and four offsets after it.
pub const CUT_TRAIN_DEEP: [usize; 5] = [14, 15, 16, 17, 18];
pub const BLOCK: usize = 3;
/// Required improvement per 14-session block (the owner's +1.0 point), scaled to a block of `BLOCK` sessions.
pub const MARGIN_PER_14: f64 = 1.0;
/// Cuts that must pass, of `CUT_TRAIN.len()`.
pub const MIN_CUTS: usize = 4;
/// An option can only be judged against a strategy that trades often enough on the unseen blocks. The five cuts share almost all of their blocks, so the evidence is the
/// busiest cut's trades (not their sum). Below this many trades no difference of a point or two between two strategies can be told from chance, so the loop does not run
/// experiments it cannot judge (the same floor as the competition judge's `MIN_TRADES`).
pub const MIN_JUDGE_TRADES: usize = 20;
/// Entry options that may be added to the plain strategy.
pub const MAX_ADDED: usize = 2;

/// The entry options a proposal may name: the bot's promotable vocabulary (checked against `live_options::is_live_token` by a test, so it cannot drift) plus
/// the research-only `ml`.
pub const OPTIONS: &[&str] = &[
    "entrywide",
    "entry0",
    "cmf",
    "vwap",
    "obv",
    "breadth",
    "er",
    "adx",
    "crash",
    "knife",
    "tstep",
    "losspause",
    "rollstop",
    "kind:ema",
    "kind:hma",
    "kind:kama",
    "kind:donchian",
    "kind:any",
    "gate:ret",
    "gate:dist",
    "gate:vol",
    "gate:atr",
    "gate:slope",
    "gate:spyvol",
    "gate:rs",
    "gate:any",
    "grade:rank",
    "grade:trend",
    "grade:both",
    "grade:risk",
    "grade:dd",
    "grade:riskdd",
    "dca:off",
    "dca:on",
    "dca:tight",
    "dca:wide",
    "top12",
    "top13",
    "top2",
    "ml",
    "base:no-trend-4h",
];

/// The token for the research base: the strategy's own trend gate removed and the holding time capped at 4 hours. The live 1-minute strategy made 4 trades on the unseen
/// blocks (nothing can be judged on that); with the gate off and the live 1-2 day hold 19 (still under the floor); with the gate off and holds up to 4 hours about 75: the improvement loop therefore judges every entry option on top of this base (owner 2026-10-20: "do what it takes to test"). It is research only
/// (`promotable` is false); what the loop keeps is applied together with it to the walk-forward and the paper trader (`in_force`).
pub const RESEARCH_BASE: &str = "base:no-trend-4h";
/// The longest hold of the research base in 1-minute bars (4 hours): the base trades inside the day, which is what the competition bot and the 1-minute candles are for.
pub const RESEARCH_HOLD_BARS: usize = 16;

/// The space the loop judges options on: the live space with the trend gate removed.
pub fn research_base(live: SearchSpace) -> SearchSpace {
    SearchSpace {
        trend_filter: false,
        hold_bars: (1, RESEARCH_HOLD_BARS),
        ..live
    }
}

/// An experiment is only "tried" for the amount of evidence it ran on (the number of scored sessions): more sessions mean new evidence, so every option may
/// be tried again.
pub fn tag(scored_sessions: usize) -> String {
    format!(" @{scored_sessions}s")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub token: String,
}

impl Proposal {
    pub fn describe(&self) -> String {
        self.token.clone()
    }
    pub fn check(&self) -> Result<()> {
        if OPTIONS.contains(&self.token.as_str()) {
            Ok(())
        } else {
            bail!("unknown entry option {:?}", self.token)
        }
    }
    /// Research-only options can never be promoted into the bot.
    pub fn promotable(&self) -> bool {
        live_options::is_live_token(&self.token)
    }
}

/// `base` with the entry option applied.
pub fn apply(base: SearchSpace, p: &Proposal) -> Result<SearchSpace> {
    p.check()?;
    let mut s = base;
    if p.token == "ml" {
        s.gates.ml_gate = true;
    } else if p.token == RESEARCH_BASE {
        s = research_base(s);
    } else {
        live_options::apply_token(&mut s, &p.token)?;
    }
    Ok(s)
}

/// Every option that really changes `base`.
pub fn all_proposals(base: SearchSpace) -> Vec<Proposal> {
    OPTIONS
        .iter()
        .map(|t| Proposal { token: (*t).into() })
        .filter(|p| apply(base, p).is_ok_and(|s| s != base))
        .collect()
}

/// Proposals not yet in `tried`.
pub fn untried(base: SearchSpace, tried: &[String]) -> Vec<Proposal> {
    all_proposals(base)
        .into_iter()
        .filter(|p| !tried.contains(&p.describe()))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    Kept,
    Discarded,
}

/// What one strategy (a search space) did out of sample over one cut's blocks.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CutStats {
    pub folds: usize,
    /// Mean out-of-sample return per block, percent.
    pub mean_block_pct: f64,
    pub stitched_pct: f64,
    pub trades: usize,
    pub wins: usize,
}

impl CutStats {
    pub fn win_rate(&self) -> Option<f64> {
        (self.trades > 0).then(|| self.wins as f64 / self.trades as f64)
    }
}

/// The owner's per-cut test: better by at least `MARGIN_PER_14` scaled to the block, win rate not lower, and positive over the stitched blocks.
pub fn cut_passes(base: &CutStats, variant: &CutStats) -> bool {
    let margin = MARGIN_PER_14 * BLOCK as f64 / 14.0;
    variant.folds > 0
        && variant.mean_block_pct - base.mean_block_pct >= margin
        && variant.stitched_pct > 0.0
        && variant
            .win_rate()
            .is_some_and(|v| base.win_rate().is_none_or(|b| v >= b))
}

/// The distinct trades behind a set of cuts (the busiest cut).
pub fn evidence_trades(stats: &[CutStats]) -> usize {
    stats.iter().map(|c| c.trades).max().unwrap_or(0)
}

/// Whether the strategy as it stands trades often enough on the unseen blocks for any option to be judged against it.
pub fn powered(base: &[CutStats]) -> bool {
    evidence_trades(base) >= MIN_JUDGE_TRADES
}

/// Records, once per evidence level, that the loop cannot judge anything yet and why. Not an experiment: `history`, `kept` and `in_force` ignore it.
pub fn note_underpowered(
    conn: &Connection,
    scored_sessions: usize,
    trades: usize,
    now_ms: i64,
) -> Result<bool> {
    let description = format!("power check{}", tag(scored_sessions));
    let exists: i64 = conn.query_row(
        "SELECT count(*) FROM experiments WHERE proposer=? AND description=?",
        rusqlite::params![PROPOSER, description],
        |r| r.get(0),
    )?;
    if exists > 0 {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO experiments(ts_ms, proposer, description, decision, reason) VALUES(?,?,?,?,?)",
        rusqlite::params![now_ms, PROPOSER, description, "inconclusive", format!("the strategy as it stands made {trades} trades on the unseen blocks (busiest cut), {MIN_JUDGE_TRADES} are needed to judge any option: no experiment was run")],
    )?;
    Ok(true)
}

/// Keep iff at least `MIN_CUTS` of the cuts pass.
pub fn judge(base: &[CutStats], variant: &[CutStats]) -> (Decision, String) {
    let passed = base
        .iter()
        .zip(variant)
        .filter(|(b, v)| cut_passes(b, v))
        .count();
    let mean =
        |x: &[CutStats]| x.iter().map(|c| c.mean_block_pct).sum::<f64>() / x.len().max(1) as f64;
    let summary = format!(
        "{passed} of {} cuts pass (need {MIN_CUTS}); mean unseen block {:+.2}% vs {:+.2}% for the current strategy; {} trades vs {}",
        base.len(),
        mean(variant),
        mean(base),
        variant.iter().map(|c| c.trades).sum::<usize>(),
        base.iter().map(|c| c.trades).sum::<usize>()
    );
    if passed >= MIN_CUTS {
        (Decision::Kept, summary)
    } else {
        (Decision::Discarded, summary)
    }
}

/// Everything a walk-forward needs besides the strategy variant.
pub struct WfCtx<'a> {
    pub b: &'a Built,
    pub sc: &'a [Vec<Option<scores::Score>>],
    pub interval: Interval,
    /// The first training windows of the cuts (`CUT_TRAIN` or `CUT_TRAIN_DEEP`).
    pub cut_train: &'a [usize],
    /// First scored candle and candles per session.
    pub first: usize,
    pub per_session: usize,
    pub samples: usize,
    pub min_trades: usize,
    pub seed: u64,
}

impl WfCtx<'_> {
    /// Scored sessions: the evidence the experiments are tagged with.
    pub fn scored_sessions(&self) -> usize {
        if self.per_session == 0 || self.first >= self.b.market.ts.len() {
            return 0;
        }
        self.b
            .market
            .ts
            .len()
            .saturating_sub(self.first.div_ceil(self.per_session) * self.per_session)
            / self.per_session
    }
    /// The unmodified live space of this candle size with this run's draws and trade floor.
    pub fn live_space(&self) -> SearchSpace {
        SearchSpace {
            samples: self.samples,
            min_trades: self.min_trades,
            ..live_options::space(self.interval, &live_options::LIVE).0
        }
    }
}

/// Out-of-sample statistics of `space` on every cut (robust rule: the bot's own winner of each fold's training window).
pub fn cut_stats(
    ctx: &WfCtx,
    sc: &[Vec<Option<scores::Score>>],
    space: SearchSpace,
) -> Result<Vec<CutStats>> {
    let total = ctx.b.market.ts.len();
    ctx.cut_train
        .iter()
        .enumerate()
        .map(|(k, &min_train)| {
            let folds = walkfwd::folds(ctx.first, total, ctx.per_session, min_train, BLOCK);
            if folds.is_empty() {
                return Ok(CutStats::default());
            }
            let rs = walkfwd::run_rules(
                ctx.b,
                sc,
                &folds,
                space,
                START_CAPITAL,
                ctx.seed + 100 * k as u64,
                &[Rule::Robust],
            )?;
            Ok(CutStats {
                folds: rs.len(),
                mean_block_pct: rs.iter().map(|r| r.return_pct()).sum::<f64>() / rs.len() as f64,
                stitched_pct: walkfwd::stitched(rs.iter().map(|r| r.return_pct())),
                trades: rs
                    .iter()
                    .map(|r| r.test_metrics.map_or(0, |m| m.trades))
                    .sum(),
                wins: rs
                    .iter()
                    .map(|r| r.test_metrics.map_or(0, |m| m.wins))
                    .sum(),
            })
        })
        .collect()
}

/// The score matrix with the network's score in `Score.ml`: refitted at every block boundary from the first scored candle on, each model trained only on rows
/// whose targets lie fully before its refit point (so the walk-forward stays causal).
pub fn with_ml(ctx: &WfCtx) -> Vec<Vec<Option<scores::Score>>> {
    let total = ctx.b.market.ts.len();
    if ctx.per_session == 0 {
        return ctx.sc.to_vec();
    }
    let starts: Vec<usize> = (0..)
        .map(|k| {
            ctx.first.div_ceil(ctx.per_session) * ctx.per_session + k * BLOCK * ctx.per_session
        })
        .take_while(|s| *s < total)
        .collect();
    let rows = learner::build_rows(ctx.b);
    let ml = learner::ml_scores(
        ctx.b,
        &rows,
        &starts,
        &learner::mlp_trainer(7, 16, 20, 0.01),
    );
    let mut out = ctx.sc.to_vec();
    for (s, row) in out.iter_mut().enumerate() {
        for (t, cell) in row.iter_mut().enumerate() {
            if let Some(score) = cell.as_mut() {
                score.ml = ml[s][t];
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub proposal: Proposal,
    pub decision: Decision,
    pub reason: String,
    pub variant: Vec<CutStats>,
    pub seconds: f64,
}

/// Runs one proposal on top of `base` (the live strategy plus the options kept so far) and logs it in `experiments`. `base_stats` are `cut_stats` of `base`.
pub fn evaluate(
    conn: &Connection,
    ctx: &WfCtx,
    base: SearchSpace,
    base_stats: &[CutStats],
    p: &Proposal,
    now_ms: i64,
) -> Result<(i64, Outcome)> {
    let mut space = apply(base, p)?;
    // the engine's ML gate re-ranks every symbol on each entry check, which is slow: those experiments draw a tenth of the settings
    if space.gates.ml_gate {
        space.samples = (space.samples / 10).max(1000);
    }
    let t = Instant::now();
    let with_ml_scores = space.gates.ml_gate.then(|| with_ml(ctx));
    let sc = with_ml_scores.as_deref().unwrap_or(ctx.sc);
    let variant = cut_stats(ctx, sc, space)?;
    let (decision, mut reason) = judge(base_stats, &variant);
    if decision == Decision::Kept && !p.promotable() {
        reason.push_str("; research only: this option can never be promoted into the bot");
    }
    let mean =
        |x: &[CutStats]| x.iter().map(|c| c.mean_block_pct).sum::<f64>() / x.len().max(1) as f64;
    conn.execute(
        "INSERT INTO experiments(ts_ms, proposer, description, oos_score, incumbent_score, decision, reason) VALUES(?,?,?,?,?,?,?)",
        rusqlite::params![
            now_ms,
            PROPOSER,
            format!("{}{}", p.describe(), tag(ctx.scored_sessions())),
            mean(&variant),
            mean(base_stats),
            if decision == Decision::Kept { "kept" } else { "discarded" },
            reason
        ],
    )?;
    let id = conn.last_insert_rowid();
    Ok((
        id,
        Outcome {
            proposal: p.clone(),
            decision,
            reason,
            variant,
            seconds: t.elapsed().as_secs_f64(),
        },
    ))
}

/// Options kept so far on this much evidence, oldest first (at most `MAX_ADDED` are ever kept).
pub fn kept(conn: &Connection, scored_sessions: usize) -> Result<Vec<String>> {
    let suffix = tag(scored_sessions);
    let mut st = conn.prepare("SELECT description FROM experiments WHERE proposer=? AND decision='kept' AND description LIKE ? ORDER BY id")?;
    let rows = st.query_map(rusqlite::params![PROPOSER, format!("%{suffix}")], |r| {
        r.get::<_, String>(0)
    })?;
    Ok(rows
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|d| d.strip_suffix(&suffix).unwrap_or(&d).to_string())
        .take(MAX_ADDED)
        .collect())
}

/// The entry options the improvement loop currently CONFIRMS: those kept on the evidence level of its latest experiment. This is what the bot's walk-forward and
/// paper trader add on top of the live setting, so a learned change shows up in every test of the bot, not only inside the loop. When more sessions arrive the loop
/// tries every option again on the new evidence; until it keeps one again, nothing from the older evidence is in force.
pub fn in_force(conn: &Connection) -> Result<Vec<String>> {
    let latest: Option<String> = conn
        .query_row("SELECT description FROM experiments WHERE proposer=? AND decision IN ('kept','discarded') ORDER BY id DESC LIMIT 1", [PROPOSER], |r| r.get(0))
        .ok();
    let sessions = latest
        .as_deref()
        .and_then(|d| d.rsplit_once(" @"))
        .and_then(|(_, tail)| tail.strip_suffix('s'))
        .and_then(|n| n.parse::<usize>().ok());
    let options = match sessions {
        Some(n) => kept(conn, n)?,
        None => Vec::new(),
    };
    // options kept on the research base are only meaningful together with it
    Ok(if options.is_empty() {
        options
    } else {
        std::iter::once(RESEARCH_BASE.to_string())
            .chain(options)
            .collect()
    })
}

/// The live space with every kept option applied.
pub fn stack(live: SearchSpace, kept: &[String]) -> Result<SearchSpace> {
    kept.iter()
        .try_fold(live, |s, t| apply(s, &Proposal { token: t.clone() }))
}

/// Proposals already tried on this much evidence, with their decision and reason, oldest first (descriptions without the tag).
pub fn history(conn: &Connection, scored_sessions: usize) -> Result<Vec<(String, String, String)>> {
    let mut st = conn.prepare("SELECT description, decision, reason FROM experiments WHERE proposer=? AND decision IN ('kept','discarded') AND description LIKE ? ORDER BY id")?;
    let suffix = tag(scored_sessions);
    let rows = st.query_map(rusqlite::params![PROPOSER, format!("%{suffix}")], |r| {
        Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(d, a, b)| (d.strip_suffix(&suffix).unwrap_or(&d).to_string(), a, b))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live() -> SearchSpace {
        SearchSpace {
            samples: 100,
            min_trades: 2,
            ..live_options::space(Interval::H5, &live_options::LIVE).0
        }
    }
    fn cs(mean: f64, stitched: f64, trades: usize, wins: usize) -> CutStats {
        CutStats {
            folds: 4,
            mean_block_pct: mean,
            stitched_pct: stitched,
            trades,
            wins,
        }
    }
    fn tok(t: &str) -> Proposal {
        Proposal { token: t.into() }
    }

    #[test]
    fn a_strategy_that_trades_too_rarely_cannot_be_used_to_judge_options() {
        let cut = |trades| CutStats {
            folds: 9,
            trades,
            ..CutStats::default()
        };
        assert!(!powered(&[cut(1), cut(0), cut(6), cut(2), cut(3)]));
        assert!(
            powered(&[cut(1), cut(21), cut(6)]),
            "the busiest cut counts, the cuts are not added up"
        );
        assert!(!powered(&[]));
        let c = Connection::open_in_memory().unwrap();
        thomas_core::store::init(&c).unwrap();
        assert!(note_underpowered(&c, 44, 6, 1).unwrap());
        assert!(
            !note_underpowered(&c, 44, 6, 2).unwrap(),
            "once per evidence level"
        );
        assert!(note_underpowered(&c, 45, 6, 3).unwrap());
        assert!(
            history(&c, 44).unwrap().is_empty()
                && kept(&c, 44).unwrap().is_empty()
                && in_force(&c).unwrap().is_empty(),
            "the note is not an experiment"
        );
    }

    #[test]
    fn every_option_is_in_the_bots_vocabulary_except_the_research_only_ml() {
        for t in OPTIONS {
            assert!(
                live_options::is_live_token(t) || *t == "ml" || *t == RESEARCH_BASE,
                "{t} is not a promotable entry option of the bot"
            );
        }
        assert!(
            !tok("ml").promotable() && !tok(RESEARCH_BASE).promotable() && tok("cmf").promotable()
        );
    }

    #[test]
    fn options_change_the_space_by_the_bots_own_mapping_and_stack() {
        let base = live();
        let c = apply(base, &tok("cmf")).unwrap();
        assert!(
            c.gates.cmf && !c.gates.vwap && c.exit == base.exit && c.direction == base.direction,
            "an entry gate only"
        );
        let both = apply(c, &tok("adx")).unwrap();
        assert!(both.gates.cmf && both.gates.adx);
        assert!(apply(base, &tok("ml")).unwrap().gates.ml_gate);
        let all = all_proposals(both);
        assert!(
            all.iter().all(|p| p.token != "cmf" && p.token != "adx"),
            "already-added options are not offered again"
        );
        assert!(all.iter().any(|p| p.token == "obv"));
        assert!(all.iter().all(|p| apply(both, p).unwrap() != both));
    }

    #[test]
    fn only_untried_options_are_offered() {
        let base = live();
        let left = untried(base, &["cmf".to_string()]);
        assert!(!left.iter().any(|p| p.token == "cmf") && left.iter().any(|p| p.token == "obv"));
    }

    #[test]
    fn a_cut_passes_only_with_the_margin_a_higher_win_rate_and_a_positive_stitched_return() {
        let base = cs(0.0, 0.0, 6, 3);
        let margin = MARGIN_PER_14 * BLOCK as f64 / 14.0;
        assert!(cut_passes(&base, &cs(margin + 0.01, 1.0, 5, 3)));
        assert!(
            !cut_passes(&base, &cs(margin - 0.01, 1.0, 5, 3)),
            "under the margin"
        );
        assert!(
            !cut_passes(&base, &cs(1.0, 1.0, 5, 2)),
            "win rate 40% is lower than 50%"
        );
        assert!(
            !cut_passes(&base, &cs(1.0, -0.5, 5, 4)),
            "negative over the stitched blocks"
        );
        assert!(
            !cut_passes(&base, &cs(1.0, 1.0, 0, 0)),
            "no trades is no evidence"
        );
        assert!(
            cut_passes(&cs(0.0, 0.0, 0, 0), &cs(1.0, 1.0, 2, 1)),
            "a base that never traded has no win rate to beat"
        );
        assert!(
            !cut_passes(&base, &CutStats::default()),
            "a cut with no folds cannot pass"
        );
    }

    #[test]
    fn the_variant_is_kept_only_when_at_least_four_of_five_cuts_pass() {
        let base = vec![cs(0.0, 0.0, 6, 3); 5];
        let good = cs(0.5, 1.0, 5, 3);
        let bad = cs(0.0, 0.0, 6, 3);
        let mix = |n: usize| -> Vec<CutStats> {
            (0..5).map(|i| if i < n { good } else { bad }).collect()
        };
        assert_eq!(judge(&base, &mix(5)).0, Decision::Kept);
        assert_eq!(judge(&base, &mix(4)).0, Decision::Kept);
        let (d, why) = judge(&base, &mix(3));
        assert_eq!(d, Decision::Discarded);
        assert!(why.contains("3 of 5 cuts pass"), "{why}");
    }

    #[test]
    fn kept_options_stack_per_amount_of_evidence_and_are_capped() {
        let c = Connection::open_in_memory().unwrap();
        thomas_core::store::init(&c).unwrap();
        let ins = |d: &str, dec: &str| {
            c.execute("INSERT INTO experiments(ts_ms,proposer,description,decision,reason) VALUES(0,?,?,?,'x')", rusqlite::params![PROPOSER, d, dec]).unwrap();
        };
        ins("cmf @20s", "kept");
        ins("obv @20s", "discarded");
        ins("adx @20s", "kept");
        ins("vwap @20s", "kept"); // a third kept option would exceed the cap
        ins("er @21s", "kept"); // other evidence: not part of this stack
        assert_eq!(kept(&c, 20).unwrap(), ["cmf", "adx"]);
        assert_eq!(kept(&c, 21).unwrap(), ["er"]);
        assert!(kept(&c, 99).unwrap().is_empty());
        let s = stack(live(), &kept(&c, 20).unwrap()).unwrap();
        assert!(s.gates.cmf && s.gates.adx && !s.gates.obv);
        let h = history(&c, 20).unwrap();
        assert_eq!(
            h.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            ["cmf", "obv", "adx", "vwap"]
        );
        assert!(
            history(&c, 5).unwrap().is_empty(),
            "other evidence: everything may be tried again"
        );
    }

    #[test]
    fn only_options_kept_on_the_latest_evidence_level_are_in_force() {
        let c = Connection::open_in_memory().unwrap();
        thomas_core::store::init(&c).unwrap();
        assert!(in_force(&c).unwrap().is_empty(), "no experiment yet");
        let add = |d: &str, dec: &str| {
            c.execute("INSERT INTO experiments(ts_ms,proposer,description,decision,reason) VALUES(0,?,?,?,'x')", rusqlite::params![PROPOSER, d, dec]).unwrap();
        };
        add("cmf @20s", "kept");
        add("obv @20s", "discarded");
        assert_eq!(
            in_force(&c).unwrap(),
            [RESEARCH_BASE, "cmf"],
            "kept on the latest evidence, with the research base it was judged on"
        );
        add("adx @30s", "discarded");
        assert!(
            in_force(&c).unwrap().is_empty(),
            "more sessions arrived and nothing is confirmed on them yet"
        );
        add("cmf @30s", "kept");
        assert_eq!(in_force(&c).unwrap(), [RESEARCH_BASE, "cmf"]);
    }
}
