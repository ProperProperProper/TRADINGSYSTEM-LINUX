//! The improvement round: `cargo run --release --example candle_improve` (the same code the statistician runs daily).
fn main() -> anyhow::Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    let r = qwen_lab::mlround::run(std::path::Path::new(&format!("{home}/Projects/qwen-lab-stocks/data/candles.sqlite3")), std::path::Path::new(&format!("{home}/Projects/qwen-lab-stocks/data/data.sqlite3")))?;
    println!("{}", r.text);
    Ok(())
}
