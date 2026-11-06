//! A3.3 on the real window with a small draw: an entry option is run through the walk-forward cuts, judged by the fixed rules, logged in `experiments`, and not
//! offered again for the same evidence (skipped without data).
use std::path::PathBuf;
use thomas_engine::improve::{self, Decision, Proposal, WfCtx};
use thomas_engine::replay::use_liquidity_model;
use thomas_engine::search::prepare_for;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name)
}

#[test]
fn an_entry_option_is_walk_forward_tested_judged_logged_and_not_offered_again() {
    if !data("candles.sqlite3").exists() || !data("spy_holdings.csv").exists() || data("candles.sqlite3").metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        eprintln!("skipped: data not present");
        return;
    }
    use_liquidity_model(true);
    let p = prepare_for(
        &data("candles.sqlite3"),
        &data("thomas.sqlite3"),
        &data("spy_holdings.csv"),
        moomoo_bot::engine::Interval::H5,
    )
    .unwrap();
    let ctx = WfCtx {
        b: &p.built,
        sc: &p.scores,
        interval: p.interval,
        cut_train: &improve::CUT_TRAIN,
        first: p.first_scored,
        per_session: p.per_session,
        samples: 300,
        min_trades: 2,
        seed: 7,
    };
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    thomas_core::store::init(&conn).unwrap();
    let live = ctx.live_space();
    let base_stats = improve::cut_stats(&ctx, ctx.sc, live).unwrap();
    assert_eq!(base_stats.len(), improve::CUT_TRAIN.len());
    assert!(
        base_stats.iter().all(|c| c.folds >= 1),
        "every cut has at least one unseen block: {base_stats:?}"
    );

    let cmf = Proposal {
        token: "cmf".into(),
    };
    let (id, out) = improve::evaluate(&conn, &ctx, live, &base_stats, &cmf, 2).unwrap();
    // the stored decision is exactly what the fixed rules give for the measured numbers
    assert_eq!(out.decision, improve::judge(&base_stats, &out.variant).0);
    let (decision, desc): (String, String) = conn
        .query_row(
            "SELECT decision, description FROM experiments WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(desc, format!("cmf{}", improve::tag(ctx.scored_sessions())));
    assert_eq!(
        decision,
        if out.decision == Decision::Kept {
            "kept"
        } else {
            "discarded"
        }
    );
    let h = improve::history(&conn, ctx.scored_sessions()).unwrap();
    assert_eq!(h.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["cmf"]);
    let tried: Vec<String> = h.into_iter().map(|x| x.0).collect();
    assert!(
        !improve::untried(live, &tried)
            .iter()
            .any(|x| x.token == "cmf")
    );
    assert!(
        improve::history(&conn, ctx.scored_sessions() + 1)
            .unwrap()
            .is_empty(),
        "more evidence: it may be tried again"
    );

    // the research-only ML option runs on causal scores and says it can never be promoted if it is kept
    let ml = Proposal { token: "ml".into() };
    let (_, out) = improve::evaluate(&conn, &ctx, live, &base_stats, &ml, 3).unwrap();
    assert_eq!(out.variant.len(), improve::CUT_TRAIN.len());
    if out.decision == Decision::Kept {
        assert!(out.reason.contains("research only"), "{}", out.reason);
    }
}
