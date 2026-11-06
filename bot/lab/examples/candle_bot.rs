//! Does letting the bot's own backtester inspire the model help? Clean data (SPY holdings only, splits handled as the bot does), only the bot's top-20
//! universe, rolling 14-session-style windows, every prediction out of sample. `cargo run --release --example candle_bot`.
use qwen_lab::{candles, mlbot, mlcandle as m};
use qwen_lab::mlcandle::{evaluate, Ridge, Sample};

fn main() -> anyhow::Result<()> {
    let path = format!("{}/Projects/qwen-lab-stocks/data/candles.sqlite3", std::env::var("HOME").unwrap_or_default());
    let db = rusqlite::Connection::open(path)?;
    let raw = candles::load(&db)?;
    let (series, notes) = candles::prepare(&raw, &candles::load_universe(&db));
    for n in notes.iter().filter(|n| n.contains("split") || n.contains("dropped")) { println!("  {n}"); }
    println!("{} symbols after cleaning (was {})", series.len(), raw.len());
    let base = m::build(&series, 4);
    let nb = m::FEATURES.len();
    let all = mlbot::extend(&series, &base);
    let top = mlbot::in_universe(&all, nb);
    println!("{} samples, {} in the top-20 universe", all.len(), top.len());
    let show = |name: &str, s: &[Sample], p: &[f64]| { let e = evaluate(s, p, 5); println!("  {name:<46} IC {:+.4} [{:+.4},{:+.4}] | top-minus-bottom {:+.1} bps [{:+.1},{:+.1}] | {} wk", e.ic, e.ic_lo, e.ic_hi, e.spread_bps, e.spread_lo, e.spread_hi, e.weeks); };
    let cols = |s: &[Sample], idx: &[usize]| -> Vec<Sample> { s.iter().map(|x| Sample { x: idx.iter().map(|&j| x.x[j]).collect(), ..x.clone() }).collect() };
    let base_idx: Vec<usize> = (0..nb).collect();
    let bot_idx: Vec<usize> = (nb..nb + mlbot::EXTRA.len()).collect();
    let both: Vec<usize> = (0..nb + mlbot::EXTRA.len()).collect();
    for (label, idx) in [("ridge: my 23 features", base_idx.clone()), ("ridge: the bot's score features only", bot_idx.clone()), ("ridge: both", both.clone())] {
        for alpha in [100.0, 1000.0, 10_000.0] {
            let data = cols(&top, &idx);
            let (s, p) = m::rolling(&data, 100, 28, 3, |t| { let r = Ridge::fit(t, alpha); Box::new(move |x| r.predict(x)) });
            show(&format!("{label} (a={alpha})"), &s, &p);
        }
    }
    // the bot's own composite ranking as a no-learning reference (what its backtests rank by): rank_value, trend_score, return score
    let (s, _) = m::rolling(&top, 100, 28, 3, |_| Box::new(|_| 0.0));
    for (name, j, sign) in [("bot rank_value (high first)", "bot_rank_value", 1.0), ("bot trend_score (high first)", "bot_trend_score", 1.0), ("bot return_score reversal", "bot_return_score", -1.0), ("bot volatility_score (low first)", "bot_volatility_score", -1.0)] {
        let k = nb + mlbot::EXTRA.iter().position(|e| *e == j).unwrap();
        show(name, &s, &s.iter().map(|x| sign * x.x[k]).collect::<Vec<_>>());
    }
    Ok(())
}
