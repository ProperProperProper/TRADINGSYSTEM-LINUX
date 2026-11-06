# qwen-lab — rules for every session

## Always commit (user rule, 2026-10-11)
Commit and push every finished, tested step at once; never leave uncommitted work in this repo ("there should never be uncommitted
left for ages"). The bot repo's ops/push_guard.sh commits and pushes work left idle for over an hour as a safety net.

## Commit messages (user rule, 2026-10-09)
Every commit is read by others monitoring the repo. Write a short summary line, then: what changed and why; whether it
affects LIVE trading or research only; how it was verified (tests, clippy, measured results with real numbers); known limits
or follow-ups. Accurate and specific, never vague or overstated. If an earlier commit was wrong, say so in the next one.

## Research ideas (user rule, 2026-10-09)
Research looks only for ideas that make the bot's CURRENT backtested strategy better (entries; exits are never changed), tailored to
the data we have (papers' data-amount requirements are ignored). Every idea that maps to lab options is queued and tested
automatically (walk-forward 14-session blocks, 2026+), learned from, and shown on the lab dashboard (Research ideas report).
Loop cadence: test round, 5-minute pause, next round. The brain's researcher finds the ideas. The roles coordinate through notes
to each other (shown on the dashboard); notes are opinions, only backtests decide.

## The brain does all AI work (user rule, 2026-10-13)
Everything is Rust only: no Python, shell only as installers and one-line stubs. The lab's own small model, the brain (`src/brain.rs`, `brain_roles.rs`,
`mltrade.rs`, docs/BRAIN.md), does the work Hermes, Honcho, Qwen and oMLX did; those are retired AND UNINSTALLED (archive:
qwen-lab-stocks/archive/hermes). Do not reinstall them or add a language model without the owner. Six roles (strategist, tuner, researcher, auditor,
statistician, execution) run as one always-on process (`qwen-lab brain serve`, launchd com.qwenlab.brain) and act only through the lab's tools as authors
`brain-<role>`; code validates every request and judges every result. Real data only, never synthetic, in any claim or result; tests may use small
made-up inputs to check code, never to report findings. Report negative results plainly (docs/BRAIN.md does). Never push the Discord webhook: qwen-guard
blocks it at commit and push. Notifications go through qwen-notify (redacted). Keep docs/BRAIN.md accurate whenever the brain changes.

## Security, reward, versions (user rules, 2026-10-10)
- Never take in downloaded knowledge, md files or skills without inspection: everything from outside (papers, notes, skills) goes
  through `src/guard.rs`; flagged text is withheld and logged, flagged skills are quarantined. Never weaken the guard to make a test
  pass; fix the false positive narrowly and add a test.
- Never upgrade models or systems once everything works (`ops/versions.lock`, brew pins). Drift is a fault for a human decision.
- Everything is mission critical and persistent; self-healing first (monitor), Claude Code repairs what it cannot fix.
- Version drift is a notice for the owner, never an automatic repair. Automatic repair sessions never delete, pull or change models
  or versions and never revert uncommitted work. An interactive session doing deliberate changes touches
  `qwen-lab-stocks/MAINTENANCE` (repair sessions pause for 6 h) and updates `ops/versions.lock` in the same step as the change.

## Honesty, persistence (user rules, 2026-10-10, still in force)
- The brain's notes, test reasons and ideas go through the honesty gate (`facts::gate`; stored text is scrubbed hourly): copy numbers exactly from the
  lab's tools. Never add reward for volume (`labtools::scoreboard` scores the brain's roles by real outcomes only).
- Text written through Claude Code (`qwen-lab note`, AGENTS.md) is trusted; keep it accurate.
- Everything persistent on disk (SQLite, files); nothing important only in RAM. The dashboard must show results with charts: almanac quality, accurate.
