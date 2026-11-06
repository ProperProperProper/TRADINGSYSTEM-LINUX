//! Validation mode (full history, no learning window): the bot's engine on the real 1-minute-based 5-hour market, replaying archived runs.
//! Skipped without the data snapshots.
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;
use thomas_engine::market::{build_on, load_symbol_data};
use thomas_engine::replay::{compute_scores, replay, use_liquidity_model};

/// id, source, first_bar_ts, last_bar_ts, start_equity, params_json, return_pct, max_drawdown_pct, trades, wins, profit_factor
type Archived = (i64, String, i64, i64, f64, String, f64, f64, i64, i64, f64);

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name)
}

#[test]
fn engine_runs_on_the_real_market_and_replays_archived_runs() {
    if !data("candles.sqlite3").exists() || !data("spy_holdings.csv").exists() || data("candles.sqlite3").metadata().map(|m| m.len()).unwrap_or(0) == 0 || !data("backtests.sqlite3").exists() {
        eprintln!("skipped: data snapshots not present");
        return;
    }
    // The archived runs were made on the bot's own moomoo bars. The merged base now carries volumes in the average unit of the two feeds (see `merge`), so the parity
    // check puts the snapshot's raw moomoo volume and turnover back into a COPY of the store: it tests the engine against the bot, not the unit conversion.
    let dir = std::env::temp_dir().join(format!("thomas-replay-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = dir.join("thomas.sqlite3");
    std::fs::copy(data("thomas.sqlite3"), &store).unwrap();
    {
        let c = Connection::open(&store).unwrap();
        c.execute(
            "ATTACH DATABASE ?1 AS snap",
            [data("candles.sqlite3").to_string_lossy().to_string()],
        )
        .unwrap();
        c.execute_batch(
            "UPDATE candles_merged SET
               volume = (SELECT s.volume FROM snap.candles s WHERE s.symbol = candles_merged.moomoo_symbol AND s.wall_ms = candles_merged.wall_ms AND s.kind = 'trades'),
               turnover = (SELECT s.turnover FROM snap.candles s WHERE s.symbol = candles_merged.moomoo_symbol AND s.wall_ms = candles_merged.wall_ms AND s.kind = 'trades')
             WHERE source = 'moomoo'",
        )
        .unwrap();
    }
    let l = load_symbol_data(&data("candles.sqlite3"), &store, &data("spy_holdings.csv")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let b = build_on(&l, moomoo_bot::engine::Interval::H5).unwrap();
    assert!(
        b.rejected.is_empty(),
        "the engine's own validation accepts every symbol"
    );
    let sc = compute_scores(&b);
    eprintln!(
        "5h market (real 1-minute bars only): {} symbols, {} candles; symbols with any score: {}",
        b.market.symbols.len(),
        b.market.ts.len(),
        sc.iter().filter(|r| r.iter().any(|x| x.is_some())).count()
    );

    let db =
        Connection::open_with_flags(data("backtests.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut st = db
        .prepare("SELECT id, source, first_bar_ts, last_bar_ts, start_equity, params_json, return_pct, max_drawdown_pct, trades, wins, profit_factor FROM bt_runs WHERE window_label LIKE '5h-fwd14%' AND trades >= 5 AND last_bar_ts >= 176000000000 ORDER BY id DESC LIMIT 40")
        .unwrap();
    let rows: Vec<Archived> = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(!rows.is_empty(), "archived runs inside the data span");
    for liq in [false, true] {
        use_liquidity_model(liq);
        let (mut ran, mut same_trades, mut same_return) = (0, 0, 0);
        for (id, src, f, la, eq, pj, ret, dd, tr, wi, pf) in &rows {
            let r = replay(&b, &sc, pj, *f, *la, *eq).expect("archived parameters replay");
            ran += 1;
            same_trades += usize::from(r.trades as i64 == *tr);
            same_return += usize::from((r.return_pct - ret).abs() < 0.01);
            if liq && (r.return_pct - ret).abs() >= 0.01 {
                eprintln!(
                    "MISMATCH run {id} [{src}] archived: ret {ret:.3}% dd {dd:.2} trades {tr} wins {wi} pf {pf:.3} | replay: ret {:.3}% dd {:.2} trades {} wins {} pf {:.3}",
                    r.return_pct, r.max_drawdown_pct, r.trades, r.wins, r.profit_factor
                );
            }
        }
        eprintln!(
            "PARITY(liquidity model {}): {ran} archived runs, same trade count {same_trades}, same return (<0.01 pt) {same_return}",
            if liq { "ON" } else { "OFF" }
        );
        if liq {
            assert!(
                same_return * 10 >= ran * 9,
                "with the liquidity model on (the archive's cost model), at least 90% of the latest runs reproduce: {same_return}/{ran}"
            );
        }
    }
}
