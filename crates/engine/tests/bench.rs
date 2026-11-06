//! How fast the bot's engine runs on this machine, and what the 20-session warm-up changes (skipped without data).
use rusqlite::{Connection, OpenFlags};
use std::{path::PathBuf, time::Instant};
use thomas_engine::market::{build_on, load_symbol_data};
use thomas_engine::replay::{compute_scores, replay, use_liquidity_model, use_warmup_sessions};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name)
}

#[test]
fn speed_and_warmup() {
    let c = data("candles.sqlite3");
    let b = data("backtests.sqlite3");
    let s = data("spy_holdings.csv");
    if !c.exists() || !b.exists() || !s.exists() || c.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        eprintln!("skipped: data snapshots not present or empty");
        return;
    }
    use_liquidity_model(true);
    let t = Instant::now();
    let l = load_symbol_data(
        &data("candles.sqlite3"),
        &data("thomas.sqlite3"),
        &data("spy_holdings.csv"),
    )
    .unwrap();
    let load = t.elapsed();
    let t = Instant::now();
    let b = build_on(&l, moomoo_bot::engine::Interval::H5).unwrap();
    let built = t.elapsed();
    let t = Instant::now();
    let sc = compute_scores(&b);
    let scored = t.elapsed();
    let first = |sc: &[Vec<Option<_>>]| {
        (0..b.market.ts.len())
            .find(|&t| sc.iter().any(|r| r[t].is_some()))
            .expect("some score")
    };
    eprintln!("LOAD STEPS {:?}", l.timings);
    eprintln!(
        "BENCH load {load:?} | build 5h market {built:?} | scores {scored:?} | first score at candle {}",
        first(&sc)
    );

    let db =
        Connection::open_with_flags(data("backtests.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut st = db
        .prepare("SELECT first_bar_ts, last_bar_ts, start_equity, params_json FROM bt_runs WHERE window_label LIKE '5h-fwd14%' AND trades >= 5 AND last_bar_ts >= 176000000000 ORDER BY id DESC LIMIT 40")
        .unwrap();
    let runs: Vec<(i64, i64, f64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    let t = Instant::now();
    for (f, la, eq, pj) in &runs {
        replay(&b, &sc, pj, *f, *la, *eq).unwrap();
    }
    let seq = t.elapsed();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let t = Instant::now();
    std::thread::scope(|s| {
        for chunk in runs.chunks(runs.len().div_ceil(threads)) {
            let (b, sc) = (&b, &sc);
            s.spawn(move || {
                for (f, la, eq, pj) in chunk {
                    replay(b, sc, pj, *f, *la, *eq).unwrap();
                }
            });
        }
    });
    let par = t.elapsed();
    eprintln!(
        "BENCH {} backtests: sequential {seq:?} ({:.0} per second) | {threads} threads {par:?} ({:.0} per second)",
        runs.len(),
        runs.len() as f64 / seq.as_secs_f64(),
        runs.len() as f64 / par.as_secs_f64()
    );

    // the research warm-up (20 sessions = 40 five-hour candles) against the bot's default
    use_warmup_sessions(20, 2);
    let sc20 = compute_scores(&b);
    let (f0, f20) = (first(&sc), first(&sc20));
    eprintln!("WARMUP first score at candle {f0} with the default, {f20} with 20 sessions");
    assert!(f20 < f0);
    use_warmup_sessions(0, 2);
}
