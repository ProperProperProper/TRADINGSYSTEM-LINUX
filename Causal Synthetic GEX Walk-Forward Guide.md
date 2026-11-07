# Causal synthetic-GEX entry and exit replay

The active implementation is `papercompmoomoo/tools/research_replay.py`. It is an offline, candidate-scoped replay for the Gainers and ORB paper bots. It does not place, amend, or cancel orders and does not change live strategy settings.

## Causal state

For each closed Moomoo 1-minute candle, the replay records typical price, friction (`volume / max(high - low, 0.01)`), a quantized strike bucket, running session VWAP as the synthetic gamma flip, and exponentially decayed friction density by strike. Defaults are a 45-bar half-life, 20-bar warm-up, and `$5` strike step.

The state evaluated for a candle is built from earlier closed candles only. The current candle is observed after its decision is made.

## Entry replay

Only symbols present in the bot's recorded candidate scope or actual trades are loaded. No broad-universe candle pull is performed.

While flat, a candidate signals when the prior-bar state is `POSITIVE_GAMMA` and the current close exceeds the prior visible call wall by the 1% breakout buffer. The signal is filled at the next available candle open, preventing lookahead.

## Exit replay

While holding a simulated entry, the replay exits on the first applicable rule:

1. Close below the causal VWAP gamma flip: `gex_regime_flip`.
2. Close below the causal call wall after its 2% stop buffer: `gex_call_wall_failure`.
3. Existing deterministic protective stop, 100% target, 70% line, intermediate, midpoint, and ratcheting high-water rules.
4. 15:00 ET session flatten.

OHLCV cannot establish intrabar ordering or executable bid/ask, so line fills are reported as estimates and never claimed as broker-exact execution.

## Research boundary

The randomized exit grid is research-only and never promotes settings into the live bots. The worker runs only after 20:00 ET on weekdays or on weekends. Candidate entry/exit results and filled-position replays are written to the normal local research output.

Tests are in `papercompmoomoo/tests/test_research_replay.py` and cover prior-bar causality, next-open entries, GEX exits, price-line exits, and deterministic grid generation.
