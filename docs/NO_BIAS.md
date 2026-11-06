# Causal market-data and decision records

Use only information available at the time of each decision. This applies to candidate discovery, entry approval, exit levels, and retrospective learning.

## Live decisions

- Record the source timestamp and retrieval time for quotes, order-book data, and candles.
- Do not treat an interval-end candle as known before that interval closes.
- Reject stale, missing, malformed, or future-dated data when a decision requires fresh evidence.
- Keep each bot's candidates, account state, and trade outcomes separate.
- Preserve the exact inputs, proposed action, validation result, order submission, and confirmed fill as distinct events.

## Retrospective analysis

- Build each example from evidence available before its cutoff. Outcomes may be labels only after the observation window ends.
- Keep training, validation, and test splits separated by session date; repeated exports of one session are not independent samples.
- Do not use future bars to set entry prices, trailing levels, or features for earlier decisions.
- Treat model explanations and historical backtests as hypotheses, not proof of live profitability.

Historical research notebooks may use other datasets. They do not change the active Moomoo-only candidate-data contract described in [`MOOMOO_DATA.md`](MOOMOO_DATA.md).
