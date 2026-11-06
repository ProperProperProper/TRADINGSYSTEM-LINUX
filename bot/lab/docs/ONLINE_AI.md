# Online AI research ensemble

The lab uses the official OpenCode CLI as the only permitted route for free hosted models. Local Qwen was retired and removed on 2026-10-17. Free models are a strict `opencode/*` allow-list; direct provider model IDs and unreviewed OpenCode model IDs are rejected before a process is started. Provider APIs and compatibility proxies are not used to bypass OpenCode authentication, quotas, policy checks, or session limits.

## Components

- `opencode.json`: intentionally uses OpenCode's stock configuration. Permission-denied custom agents are not used because OpenCode currently misclassifies some of their free-tier requests as external traffic.
- `src/teacher_engine.rs`: runs the built-in `plan` agent from an empty runtime directory, puts the researcher/validator role in the prompt, registers the available free models, chooses the least-used healthy model, penalises failures fourfold, normalises replies to `{candidate, reason}`, and requires two independent models to agree.
- Free-tier routing fails closed: only entries in `FREE_MODELS` with the `opencode/` namespace can reach the official CLI. Authentication, quota, policy, or session failures never fall through to a direct provider API.
- The exact “free tier can only be used from within OpenCode” rejection opens a circuit breaker immediately; the same cycle does not retry the other free models. Re-authenticate with `opencode auth login opencode` before retrying.
- `src/online_brain.rs`: persistent research-brain integration. It stores decisions, verified outcomes and reusable option skills in SQLite, gives every model the same shared memory (brain state, verified outcomes, option skills, recent actions and earlier model answers), and routes agreed proposals through `lab_queue_test`.
- Claude Code is the preferred final validator. When it is unavailable, the fastest free model (`opencode/deepseek-v4.1-flash-free`) decides. Codex is reserved for interactive use and is never launched by unattended automation. The chosen validator is recorded with the proposal.

## Install

Install OpenCode from its official installer, connect an OpenCode Console credential, then verify the stock configuration:

```sh
curl -fsSL https://opencode.ai/v2/install | bash
~/.opencode/bin/opencode --version
~/.opencode/bin/opencode auth login opencode
~/.opencode/bin/opencode models
```

Claude Code may be installed and signed in for final validation. The lab itself remains deterministic when AI service is unavailable: the online proposal is skipped and the Rust strategist continues. Codex credentials are never consumed by the lab.

## Checks and balances

1. Models receive training-side stored-candle evidence only.
2. Two independently selected OpenCode models must name the same candidate (a name and its index are equivalent).
3. Claude validates grounding and safety; when it is unavailable the fastest OpenCode free model decides.
4. The normal security/fact gates validate the reason.
5. `lab_queue_test` permits only known strategy options and paper research.
6. Walk-forward tests judge outcomes; only those code verdicts update trusted memory and skills.
7. Existing promotion rules remain the only path to paper settings. The ensemble cannot place orders, alter exits, or promote itself.

## Persistent data

The SQLite memory database contains provider health/load (`teacher_models`), per-model responses (`online_model_responses`), the complete decision audit trail (`online_turns`), verified training-side outcome memory (`online_memory`), training-only option evidence and use counters (`online_training_skills`), and current brain state (`online_brain`). Reconciliation learns from paired gain plus clean/noisy training returns; it never uses candidate/validated status. Legacy `online_skills` rows and pre-v2 responses are retained for audit but excluded from decision evidence. On first use, old `qwen_*` tables are copied once into the neutral legacy tables and retained as a recoverable archive. No Qwen runtime, model, adapter, trainer, or active code module remains.

“Winner” never means live or paper profit. It means a deterministic stored-candle backtest and walk-forward verdict produced by `strategy::leaderboard`. Live and paper results may be displayed and monitored, but they cannot label a winner, update trusted lessons, or increase skill confidence. No model can guarantee profitable trading.

## Troubleshooting

- HTTP 403 saying the free tier must run “within OpenCode” means the OpenCode Console credential needs refreshing or OpenCode rejected the client/agent shape. Run `opencode auth login opencode`; automation uses the stock `plan` agent from an empty directory because permission-denied custom agents trigger this known OpenCode bug.
- A provider failure increments its failure score and cools it down. Other free models continue carrying the load.
- Model disagreement is expected and safe: no proposal is committed.
- Claude weekly-limit messages route validation to the OpenCode free validator and are recorded in the rationale; they never launch Codex.
