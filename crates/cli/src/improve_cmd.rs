//! `thomas improve`: A3.3. Takes the next experiment from the untried proposals (a fixed list, in its listed order); the harness runs it (search, guards, keep
//! rules, registry) and logs it. No number is ever supplied from outside the list.
use anyhow::Result;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use thomas_core::store;
use thomas_engine::improve::{self, Decision};
use thomas_engine::search;

#[allow(clippy::too_many_arguments)]
pub fn run(
    sdb: &Path,
    snapshot: &Path,
    holdings: &Path,
    samples: usize,
    seed: u64,
    min_trades: usize,
    interval: &str,
    deep: bool,
    rounds: usize,
) -> Result<()> {
    let interval = moomoo_bot::engine::Interval::ALL
        .iter()
        .copied()
        .find(|i| i.label() == interval)
        .ok_or_else(|| anyhow::anyhow!("unknown candle size '{interval}'"))?;
    let search::Prepared {
        built: b,
        scores: sc,
        first_scored: first,
        per_session,
        ..
    } = if deep {
        search::prepare_deep(snapshot, sdb, holdings, interval, 60)?
    } else {
        search::prepare_for(snapshot, sdb, holdings, interval)?
    };
    let ctx = improve::WfCtx {
        b: &b,
        sc: &sc,
        interval,
        cut_train: if deep {
            &improve::CUT_TRAIN_DEEP
        } else {
            &improve::CUT_TRAIN
        },
        first,
        per_session,
        samples,
        min_trades,
        seed,
    };
    let conn = store::open(sdb)?;
    let now = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64)
    };
    let live = improve::research_base(ctx.live_space());
    let sessions = ctx.scored_sessions();
    let mut cached: Option<(Vec<String>, Vec<improve::CutStats>)> = None;
    for round in 1..=rounds {
        // kept options stack: the next experiment is judged on top of them, against the strategy as it stands
        let kept = improve::kept(&conn, sessions)?;
        if kept.len() >= improve::MAX_ADDED {
            println!(
                "{} options are already added ({}): the limit",
                kept.len(),
                kept.join(", ")
            );
            break;
        }
        let base = improve::stack(live, &kept)?;
        if cached.as_ref().is_none_or(|c| c.0 != kept) {
            println!(
                "measuring the current strategy ({}) on unseen walk-forward blocks over {sessions} scored sessions",
                if kept.is_empty() {
                    "plain".into()
                } else {
                    kept.join(" + ")
                }
            );
            cached = Some((kept.clone(), improve::cut_stats(&ctx, &sc, base)?));
        }
        let base_stats = cached.as_ref().map(|c| c.1.clone()).unwrap_or_default();
        if !improve::powered(&base_stats) {
            let trades = improve::evidence_trades(&base_stats);
            let first = improve::note_underpowered(&conn, sessions, trades, now())?;
            println!(
                "the strategy as it stands made {trades} trades on the unseen blocks (busiest cut); {} are needed to judge any option, so no experiment is run{}",
                improve::MIN_JUDGE_TRADES,
                if first { " (noted)" } else { "" }
            );
            break;
        }
        let base_mean = base_stats.iter().map(|c| c.mean_block_pct).sum::<f64>()
            / base_stats.len().max(1) as f64;
        let history = improve::history(&conn, sessions)?;
        let tried: Vec<String> = history.iter().map(|h| h.0.clone()).collect();
        let options = improve::untried(base, &tried);
        if options.is_empty() {
            println!("every proposal has been tried");
            break;
        }
        let proposal = options[0].clone();
        println!(
            "round {round}: next experiment {} ({} untried; base mean unseen block {base_mean:+.2}%)",
            proposal.describe(),
            options.len()
        );
        let (_, out) = improve::evaluate(&conn, &ctx, base, &base_stats, &proposal, now())?;
        println!(
            "  {} in {:.0} s: {}",
            if out.decision == Decision::Kept {
                "KEPT"
            } else {
                "discarded"
            },
            out.seconds,
            out.reason
        );
    }
    Ok(())
}
