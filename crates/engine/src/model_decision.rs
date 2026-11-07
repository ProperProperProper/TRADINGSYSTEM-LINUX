//! Validation for the model-to-Rust decision envelope.
//!
//! This is deliberately only a parser.  A validated envelope is still data;
//! the replay, capital guard, and order boundaries decide what (if anything)
//! may happen next.

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Decision {
    Enter,
    Skip,
    Exit,
    Abstain,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Symbols {
    pub moomoo: String,
    pub external: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema: u32,
    pub decision: Decision,
    pub symbol: Symbols,
    pub signal_ts_ms: i64,
    pub confidence: f64,
    pub reason: String,
    #[serde(default)]
    pub thinking: Option<String>,
    pub brain_ids: Vec<i64>,
    #[serde(default)]
    pub board_ids: Vec<i64>,
    pub data_cutoff: String,
    pub strategy_version: String,
    pub model_version: String,
}

pub fn parse(input: &str, now_ms: i64) -> Result<Envelope> {
    let envelope: Envelope =
        serde_json::from_str(input).context("model decision is not valid JSON")?;
    ensure!(
        envelope.schema == 1,
        "unsupported model decision schema {}",
        envelope.schema
    );
    ensure!(
        !envelope.symbol.moomoo.trim().is_empty(),
        "moomoo symbol is required"
    );
    ensure!(
        !envelope.symbol.external.trim().is_empty(),
        "external symbol is required"
    );
    ensure!(
        envelope.signal_ts_ms <= now_ms,
        "model decision is from the future"
    );
    ensure!(
        envelope.confidence.is_finite() && (0.0..=1.0).contains(&envelope.confidence),
        "confidence must be finite and between 0 and 1"
    );
    ensure!(
        envelope.decision == Decision::Abstain || envelope.confidence > 0.0,
        "non-abstain decisions require positive confidence; abstain instead"
    );
    ensure!(
        !envelope.reason.trim().is_empty(),
        "decision reason is required"
    );
    ensure!(
        !envelope.brain_ids.is_empty(),
        "brain provenance is required"
    );
    ensure!(
        envelope.brain_ids.iter().all(|id| *id > 0),
        "brain IDs must be positive"
    );
    ensure!(
        envelope.board_ids.iter().all(|id| *id > 0),
        "board IDs must be positive"
    );
    ensure!(
        !envelope.data_cutoff.trim().is_empty(),
        "data cutoff is required"
    );
    ensure!(
        !envelope.strategy_version.trim().is_empty(),
        "strategy version is required"
    );
    ensure!(
        !envelope.model_version.trim().is_empty(),
        "model version is required"
    );
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> &'static str {
        r#"{"schema":1,"decision":"abstain","symbol":{"moomoo":"US.X","external":"X"},"signal_ts_ms":100,"confidence":0.4,"reason":"insufficient evidence","brain_ids":[7],"board_ids":[],"data_cutoff":"2026-10-22T00:00:00Z","strategy_version":"2","model_version":"qwen-base"}"#
    }

    #[test]
    fn parses_valid_envelope() {
        let parsed = parse(valid(), 100).unwrap();
        assert_eq!(parsed.decision, Decision::Abstain);
        assert_eq!(parsed.symbol.external, "X");
    }

    #[test]
    fn refuses_future_signal() {
        assert!(parse(valid().replace("100", "101").as_str(), 100).is_err());
    }

    #[test]
    fn refuses_missing_provenance_and_unknown_fields() {
        assert!(
            parse(
                &valid().replace("\"brain_ids\":[7]", "\"brain_ids\":[]"),
                100
            )
            .is_err()
        );
        assert!(parse(&valid().replace("}", ",\"extra\":true}"), 100).is_err());
    }

    #[test]
    fn refuses_action_with_zero_confidence() {
        assert!(
            parse(
                &valid()
                    .replace("\"abstain\"", "\"enter\"")
                    .replace("0.4", "0.0"),
                100
            )
            .is_err()
        );
    }
}
