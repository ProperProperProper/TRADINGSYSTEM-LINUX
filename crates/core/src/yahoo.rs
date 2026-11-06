//! Yahoo Finance chart API client: a secondary, real-data source for moomoo gaps and cross-checks.
//! Responses are parsed into numbers only and sanity-checked; no text from the network is ever used
//! as an instruction. Yahoo serves 1-minute bars only for the last 60 days.
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::time::Duration;

use crate::candles::Bar;

pub const SOURCE: &str = "yahoo";
/// Yahoo rejects 1m requests that start more than 60 days back; stay safely inside.
pub const MAX_1M_DAYS: i64 = 58;

pub enum Fetch {
    /// `returned` is the symbol Yahoo says the bars belong to (must equal what we asked for).
    Bars {
        returned: String,
        name: String,
        bars: Vec<Bar>,
    },
    /// Yahoo has no data for this request (outside its 1m window, or unknown symbol).
    Unavailable(String),
}

pub struct Client {
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .user_agent("Mozilla/5.0")
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    /// Session open times (ms, UTC) of a symbol's daily bars in [from_s, to_s): Yahoo stamps a US daily bar with the session's 09:30 ET open,
    /// so these are the real trading days (holidays are absent). Used to build the trading calendar.
    pub fn daily_opens(&self, symbol: &str, from_s: i64, to_s: i64) -> Result<Vec<i64>> {
        let url = format!(
            "https://query1.finance.yahoo.com/v8/finance/chart/{symbol}?interval=1d&period1={from_s}&period2={to_s}"
        );
        let v: Value = self
            .http
            .get(&url)
            .send()
            .with_context(|| format!("yahoo daily request for {symbol}"))?
            .json()?;
        if !v["chart"]["error"].is_null() {
            bail!(
                "yahoo daily: {}",
                v["chart"]["error"]["description"]
                    .as_str()
                    .unwrap_or("error")
            );
        }
        let results = v["chart"]["result"]
            .as_array()
            .context("yahoo daily response missing chart result")?;
        let Some(result) = results.first() else {
            return Ok(Vec::new());
        };
        let ts = result["timestamp"]
            .as_array()
            .context("yahoo daily response missing timestamps")?;
        Ok(ts
            .iter()
            .filter_map(Value::as_i64)
            .filter_map(|t| t.checked_mul(1000))
            .collect())
    }

    /// 1-minute bars in [from_s, to_s) (unix seconds). Bar timestamps are bar-start, UTC.
    pub fn bars_1m(&self, symbol: &str, from_s: i64, to_s: i64) -> Result<Fetch> {
        let symbol = &symbol.replace('.', "-"); // Yahoo writes BRK.B as BRK-B
        let url = format!(
            "https://query1.finance.yahoo.com/v8/finance/chart/{symbol}?interval=1m&period1={from_s}&period2={to_s}"
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .with_context(|| format!("yahoo request for {symbol}"))?;
        let status = resp.status();
        let v: Value = resp.json()?;
        parse_bars_response(status, v, symbol)
    }
}

fn parse_bars_response(status: reqwest::StatusCode, v: Value, symbol: &str) -> Result<Fetch> {
    let chart = &v["chart"];
    if !chart["error"].is_null() {
        return Ok(Fetch::Unavailable(
            chart["error"]["description"]
                .as_str()
                .unwrap_or("yahoo error")
                .chars()
                .take(200)
                .collect(),
        ));
    }
    ensure!(status.is_success(), "yahoo http {status} for {symbol}");
    let r = chart["result"]
        .as_array()
        .and_then(|results| results.first())
        .context("yahoo response missing chart result")?;
    let ts = r["timestamp"].as_array().cloned().unwrap_or_default();
    let q = &r["indicators"]["quote"][0];
    let col = |k: &str| q[k].as_array().cloned().unwrap_or_default();
    let (o, h, l, c, vol) = (
        col("open"),
        col("high"),
        col("low"),
        col("close"),
        col("volume"),
    );
    let mut out = Vec::with_capacity(ts.len());
    for (i, t) in ts.iter().enumerate() {
        let (Some(t), Some(open), Some(high), Some(low), Some(close)) = (
            t.as_i64(),
            o.get(i).and_then(Value::as_f64),
            h.get(i).and_then(Value::as_f64),
            l.get(i).and_then(Value::as_f64),
            c.get(i).and_then(Value::as_f64),
        ) else {
            continue;
        };
        let volume = vol.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        let Some(ts_ms) = t.checked_mul(1000) else {
            continue;
        };
        let bar = Bar {
            ts_ms,
            open,
            high,
            low,
            close,
            volume,
        };
        if sane(&bar) {
            out.push(bar);
        }
    }
    let returned = r["meta"]["symbol"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(20)
        .collect();
    let name = r["meta"]["longName"]
        .as_str()
        .or(r["meta"]["shortName"].as_str())
        .unwrap_or("")
        .chars()
        .take(80)
        .collect();
    Ok(Fetch::Bars {
        returned,
        name,
        bars: out,
    })
}

/// Rejects anything that is not a plausible price bar.
pub(crate) fn sane(b: &Bar) -> bool {
    [b.open, b.high, b.low, b.close]
        .iter()
        .all(|p| p.is_finite() && *p > 0.0 && *p < 1.0e7)
        && b.high >= b.low
        && b.high >= b.open.max(b.close)
        && b.low <= b.open.min(b.close)
        && b.volume.is_finite()
        && b.volume >= 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
        Bar {
            ts_ms: 0,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
        }
    }

    #[test]
    fn sane_accepts_valid_and_rejects_bad() {
        assert!(sane(&bar(10.0, 11.0, 9.0, 10.5, 100.0)));
        assert!(!sane(&bar(10.0, 9.0, 9.5, 10.0, 1.0)), "high below low");
        assert!(!sane(&bar(f64::NAN, 11.0, 9.0, 10.0, 1.0)));
        assert!(!sane(&bar(0.0, 1.0, 0.0, 1.0, 1.0)), "non-positive price");
        assert!(!sane(&bar(10.0, 11.0, 9.0, 10.0, -5.0)), "negative volume");
    }

    #[test]
    fn malformed_chart_payload_is_an_error_not_an_empty_success() {
        assert!(
            parse_bars_response(
                reqwest::StatusCode::OK,
                serde_json::json!({"chart": {}}),
                "AAPL"
            )
            .is_err()
        );
    }
}
