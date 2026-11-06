//! Prints the pre-registered candle machine-learning test on the lab's candle store: `cargo run --release --example candle_ml`.
use qwen_lab::{candles, mlcandle};
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| format!("{}/Projects/qwen-lab-stocks/data/candles.sqlite3", std::env::var("HOME").unwrap_or_default()));
    let db = rusqlite::Connection::open(path)?;
    let series = candles::load(&db)?;
    println!("{}", mlcandle::report(&series, &candles::load_costs(&db)).text);
    Ok(())
}
