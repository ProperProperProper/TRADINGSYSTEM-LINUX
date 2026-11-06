# Agent handoff

Follow [`AGENTS.md`](AGENTS.md) for project boundaries and workflow. Start with [`README.md`](README.md), [`STATE.md`](STATE.md), and [`TASKS.md`](TASKS.md).

The active system is two simulated Moomoo bots sharing one `COMPETITION` account: Gainers owns the US hottest-gainers leaderboard request and publishes a local snapshot that ORB consumes. Both filter Nasdaq-listed symbols before fetching candidate data. A shared allocator coordinates symbols and buying power. The Rust strategy under `bot/` is archived. Preserve ignored runtime data and immutable decision/fill evidence.

The live exit worker applies deterministic position-level rules and a one-hour-before-close flatten. Every exit line, including 70%, ratchets upward only; the 70% line tracks 70% of the current full-target line. Its legacy `codex_exit_worker.py` name does not indicate discretionary Codex decisions for Gainers or ORB. Qwen learning is shelved indefinitely and its automation has been removed; preserve the cognitive brain and ThomasAI decision components. Read [`docs/EXIT_WORKER.md`](docs/EXIT_WORKER.md) before changing these boundaries.

Use New York session dates and ET labels for premarket (04:00–09:30) and regular (09:30–16:00) charts. Moomoo minute candles are end-labeled; place them over the true interval and preserve provider timestamps. Keep live overlays within their chart's session.
