//! The v2 candle experiment: `cargo run --release --example candle_v2`.
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| format!("{}/Projects/qwen-lab-stocks/data/candles.sqlite3", std::env::var("HOME").unwrap_or_default()));
    let series = qwen_lab::candles::load(&rusqlite::Connection::open(path)?)?;
    println!("{}", qwen_lab::mlcandle2::report(&series));
    Ok(())
}
