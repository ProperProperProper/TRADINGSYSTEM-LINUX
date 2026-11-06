//! THE LIVE STRATEGY'S ENTRY OPTIONS: research options that passed the lab's full validation and were promoted into the live
//! strategy (owner 2026-10-10: "if it passes always go live with the strat; update backtester and paper"; "claude makes code
//! changes, not anything else"; "always update the strat in repo and put comments on what is different and why").
//!
//! How a promotion happens: the lab's code (qwen-lab `promote`) finds a variant that passes the full validation (every block
//! offset) and writes a promotion request; the hourly promotion job (ops/promote.sh) starts a Claude Code session, only while the
//! US market is closed (never disrupting paper trading), which changes [`LIVE`] below WITH A COMMENT (what differs from the
//! previous strategy, the validation numbers, why), adds a line to docs/STRATEGY.md, runs the tests, deploys and pushes.
//!
//! Everything uses [`space`]: the live search (and therefore the paper account, which trades the search's settings), the
//! console's stability and walk-forward tests, and the backtester (`examples/variant_study`), so what is measured and what trades
//! is always the same strategy. Exits are never touched: only entry options can be promoted ([`is_live_token`]).

use super::walkforward::{self, SearchSpace};
use super::xs::{self, XsParams};
use super::Interval;
use anyhow::{bail, Context, Result};
use serde::Serialize;

/// The live strategy: its interval, the promoted entry options, and why.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct LiveStrategy {
    /// Interval key the options apply to; other intervals keep the plain strategy.
    pub interval: &'static str,
    /// Promoted entry options (tokens of the lab's vocabulary).
    pub options: &'static [&'static str],
    /// The promoted variant and who found it ("planner (code)", "hermes-strategist", ...).
    pub variant: &'static str,
    pub source: &'static str,
    /// When it went live (UTC date) and the commit that did it is the git history of this constant.
    pub since: &'static str,
    /// What differs from the previous strategy and why (the validation numbers), one paragraph.
    pub why: &'static str,
}

// ---- CHANGE LOG (newest first). Every change of LIVE adds an entry here AND in docs/STRATEGY.md:
// date | old options -> new options | variant (source) | full validation (cuts passed, unseen-block %/block vs the previous
// strategy per cut) | why.
// 2026-10-10 | (none) | the plain strategy; promotion mechanism added, nothing promoted yet.

/// The live strategy (see the change log above).
pub const LIVE: LiveStrategy = LiveStrategy {
    interval: "5h",
    options: &[],
    variant: "",
    source: "",
    since: "2026-10-10",
    why: "The plain strategy: no option has passed the full validation yet.",
};

/// Whether `tok` may be part of the live strategy: entry-side options only (gates, confirmations, trend kind, graduated sizing,
/// entry band, basket size, DCA and tuned entry ranges). Never a signal, direction, exit, universe or research switch.
pub fn is_live_token(tok: &str) -> bool {
    matches!(
        tok,
        "entrywide"
            | "entry0"
            | "cmf"
            | "vwap"
            | "obv"
            | "breadth"
            | "er"
            | "adx"
            | "crash"
            | "knife"
            | "tstep"
            | "losspause"
            | "rollstop"
            | "kind:ema"
            | "kind:hma"
            | "kind:kama"
            | "kind:donchian"
            | "kind:any"
            | "gate:ret"
            | "gate:dist"
            | "gate:vol"
            | "gate:atr"
            | "gate:slope"
            | "gate:spyvol"
            | "gate:rs"
            | "gate:any"
            | "grade:rank"
            | "grade:trend"
            | "grade:both"
            | "grade:risk"
            | "grade:dd"
            | "grade:riskdd"
            | "dca:off"
            | "dca:on"
            | "dca:tight"
            | "dca:wide"
            | "top12"
            | "top13"
            | "top1"
            | "top2"
    ) || tok.starts_with("tune:margin=")
        || tok.starts_with("tune:dca=")
}

/// Applies one entry option to a search space (the one mapping shared by the live search and the backtester).
pub fn apply_token(space: &mut SearchSpace, tok: &str) -> Result<()> {
    match tok {
        "top12" => space.top = (1, 2),
        "top13" => space.top = (1, 3),
        "top1" => space.top = (1, 1),
        "top2" => space.top = (2, 2),
        "entrywide" => {
            space.margin_pctl = (30.0, 99.9);
            space.min_margin = Some(0.0);
            space.exit_pctl = Some(walkforward::MARGIN_PCTL);
        }
        "entry0" => {
            space.margin_pctl = (0.0, 99.9);
            space.min_margin = Some(0.0);
            space.exit_pctl = Some(walkforward::MARGIN_PCTL);
        }
        "cmf" => space.gates.cmf = true,
        "vwap" => space.gates.vwap = true,
        "obv" => space.gates.obv = true,
        "breadth" => space.gates.breadth = true,
        "er" => space.gates.er = true,
        "adx" => space.gates.adx = true,
        "crash" => space.gates.crash = true,
        "knife" => space.gates.knife = true,
        "tstep" => space.gates.tstep = true,
        "losspause" => space.gates.loss_pause = true,
        "rollstop" => space.gates.roll_stop = true,
        "kind:ema" => space.gates.kind = Some(xs::TrendKind::Ema),
        "kind:hma" => space.gates.kind = Some(xs::TrendKind::Hma),
        "kind:kama" => space.gates.kind = Some(xs::TrendKind::Kama),
        "kind:donchian" => space.gates.kind = Some(xs::TrendKind::Donchian),
        "kind:any" => space.gates.kind_any = true,
        "gate:ret" => space.gates.gate = Some(xs::GateFeature::Ret),
        "gate:dist" => space.gates.gate = Some(xs::GateFeature::DistAvg),
        "gate:vol" => space.gates.gate = Some(xs::GateFeature::VolRatio),
        "gate:atr" => space.gates.gate = Some(xs::GateFeature::Atr),
        "gate:slope" => space.gates.gate = Some(xs::GateFeature::RefSlope),
        "gate:spyvol" => space.gates.gate = Some(xs::GateFeature::RefVol),
        "gate:rs" => space.gates.gate = Some(xs::GateFeature::RelStrength),
        "gate:any" => space.gates.gate_any = true,
        "grade:rank" => space.gates.grade_rank = true,
        "grade:trend" => space.gates.grade_trend = true,
        "grade:risk" => space.gates.grade_risk = true,
        "grade:dd" => space.gates.grade_dd = true,
        "grade:riskdd" => {
            space.gates.grade_risk = true;
            space.gates.grade_dd = true;
        }
        "grade:both" => {
            space.gates.grade_rank = true;
            space.gates.grade_trend = true;
        }
        "dca:off" => space.gates.dca = Some((1.0, 5.0, 20.0)),
        "dca:on" => space.gates.dca = Some((0.0, 5.0, 20.0)),
        "dca:tight" => space.gates.dca = Some((0.0, 2.0, 6.0)),
        "dca:wide" => space.gates.dca = Some((0.0, 10.0, 25.0)),
        t if t.starts_with("tune:") => {
            let (what, range) = t[5..].split_once('=').context("tune:<what>=<lo>-<hi>")?;
            let (lo, hi) = range.split_once('-').context("tune range lo-hi")?;
            let (lo, hi): (f64, f64) = (lo.parse()?, hi.parse()?);
            anyhow::ensure!(
                lo.is_finite() && hi.is_finite() && lo < hi && lo >= 0.0,
                "bad tune range {t}"
            );
            match what {
                "margin" => {
                    anyhow::ensure!(hi <= 10.0, "margin range beyond 10%");
                    space.gates.margin = Some((lo, hi))
                }
                "dca" => {
                    anyhow::ensure!(hi <= 40.0, "dca range beyond 40%");
                    space.gates.dca = Some((0.0, lo, hi))
                }
                other => bail!("unknown tune target {other}"),
            }
        }
        other => bail!("not a live entry option: {other}"),
    }
    Ok(())
}

/// The search space of `interval` under `live` (the plain `space_for` when its options do not apply), and the options used.
pub fn space(interval: Interval, live: &LiveStrategy) -> (SearchSpace, Vec<String>) {
    let mut s = walkforward::space_for(interval);
    let mut used = Vec::new();
    if live.interval == interval.key() {
        for t in live.options {
            if apply_token(&mut s, t).is_ok() {
                used.push(t.to_string());
            }
        }
    }
    (s, used)
}

/// Whether recorded settings fit `space`: every entry option the space switches on must be present in the settings (settings
/// recorded before a promotion lack the new gate and would trade the old strategy, so they are not re-tested).
pub fn params_fit(p: &XsParams, space: &SearchSpace) -> bool {
    let g = space.gates;
    let r = &p.risk;
    !(g.cmf && r.cmf_bars.is_none()
        || g.vwap && r.vwap_bars.is_none()
        || g.obv && r.obv_bars.is_none()
        || g.breadth && r.breadth_min_pct.is_none()
        || g.er && r.er_min.is_none()
        || g.adx && r.adx_min.is_none()
        || g.crash && r.crash_pct.is_none()
        || g.knife && r.knife_drop_pct.is_none()
        || g.tstep && r.entry_step.is_none()
        || (g.kind.is_some() || g.kind_any) && r.entry_kind.is_none()
        || (g.gate.is_some() || g.gate_any) && r.entry_gate.is_none()
        || g.grade_rank && r.grade_rank.is_none()
        || g.grade_trend && r.grade_trend.is_none()
        || g.grade_risk && r.grade_risk.is_none()
        || g.grade_dd && r.grade_dd.is_none()
        || g.loss_pause && r.loss_pause.is_none()
        || g.roll_stop && r.roll_stop.is_none()
        || g.ml_gate && r.ml_gate.is_none()
        || g.margin.is_some() && r.trend_exit_margin_pct.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: LIVE holds only entry options; options apply only to their interval; old settings without a promoted gate do not
    /// fit the new space.
    #[test]
    fn live_options_apply_to_their_interval_and_filter_old_settings() {
        assert!(
            LIVE.options.iter().all(|t| is_live_token(t)),
            "LIVE may hold entry options only"
        );
        let o = LiveStrategy {
            options: &["obv"],
            ..LIVE
        };
        let (s5, used) = space(Interval::H5, &o);
        assert!(s5.gates.obv && used == vec!["obv".to_string()]);
        let (s1, used1) = space(Interval::H1, &o);
        assert!(
            !s1.gates.obv && used1.is_empty(),
            "other intervals keep the plain strategy"
        );
        assert!(
            space(
                Interval::H5,
                &LiveStrategy {
                    options: &[],
                    ..LIVE
                }
            )
            .0 == walkforward::space_for(Interval::H5)
        );
        let mut rs = walkforward::space_for(Interval::H5);
        apply_token(&mut rs, "gate:rs").unwrap();
        assert_eq!(rs.gates.gate, Some(xs::GateFeature::RelStrength));
        let drawn = walkforward::sample_space(3, 200, &walkforward::tests::test_band(), rs);
        assert!(drawn.iter().all(|p| matches!(p.risk.entry_gate, Some(g) if g.feature == xs::GateFeature::RelStrength && g.threshold.abs() <= 3.0)));
        assert!(
            !is_live_token("sig:Rsi")
                && !is_live_token("contra")
                && !is_live_token("univ60")
                && !is_live_token("keep")
        );
        let mut p: XsParams =
            walkforward::sample_params(7, 1, &walkforward::tests::test_band()).remove(0);
        p.risk.obv_bars = None;
        assert!(params_fit(&p, &walkforward::space_for(Interval::H5)));
        assert!(
            !params_fit(&p, &s5),
            "settings without the OBV gate do not fit"
        );
        p.risk.obv_bars = Some(40);
        assert!(params_fit(&p, &s5));
    }
}
