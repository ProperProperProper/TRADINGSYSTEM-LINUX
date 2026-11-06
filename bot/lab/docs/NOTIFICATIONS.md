# Discord change notifications

Every change the automation makes is posted to the owner's Discord by `qwen-notify` (a small Rust binary, `src/bin/qwen-notify.rs`, logic in
`src/notify.rs`). Without a webhook it does nothing, so nothing depends on it.

## Setup (once)
1. Discord: channel settings -> Integrations -> Webhooks -> New Webhook -> Copy Webhook URL.
2. Store it OUTSIDE every repo, readable only by you (copy the URL first, then run this in the terminal; it keeps the URL out of chat and shell history):
   `mkdir -p ~/.config/qwen-lab && pbpaste > ~/.config/qwen-lab/discord_webhook && chmod 600 ~/.config/qwen-lab/discord_webhook`
3. `bash ops/resource_safety/install.sh` builds and installs `~/.local/bin/qwen-notify` (signed with the stable identifier `com.qwenlab.notify`, so
   the firewall rule you approve once keeps applying; the first post will trigger one LuLu prompt: choose Allow for discord.com).
4. Test: `qwen-notify --title Test "hello from the lab"`.

## What is posted
| Source | Message |
|---|---|
| `ops/push_guard.sh` (every commit of the bot and lab repos, via the post-commit hooks and both monitors) | commits pushed (hash + subject), or PUSH FAILED |
| `ops/lab_monitor.sh` (every 15 min) | each fix it made (restarted job, lab/bot redeployed) and each fault it found |
| bot `ops/monitor.sh` | the same events that raise a macOS notification (OpenD down, fault fixed, daily review) |
| `ops/publish_state.sh` (bot repo) | the brain's reports, the ML logs and Claude's notes published |
| `qwen-ops watch` (auto-heal) | every automatic repair (restart, swap action) |
| `qwen-ops watchdog` | when the Claude fixer starts, and its result (first 900 characters) when it finishes |
| the brain's agents | the steward switching the ML model off/on, an adopted improvement, a variant passing validation, stale data, a paper-account rule nearly broken, a promotion packet |
| the daily digest (statistician) | one short status: model state, recent IC, trials, results, paper account |

## Safety
* Only `discord.com`, `discordapp.com`, `canary.discord.com` and `ptb.discord.com` webhook URLs are accepted, so the command cannot post text anywhere else.
* `qwen-guard` (Rust) installs pre-commit and pre-push hooks in the lab and bot repos that block any commit or push adding a real-shaped webhook URL or the webhook file (even `--no-verify` is caught at push).
* The URL is never printed or logged, and is kept out of error messages.
* Redaction before sending: runs of 9+ digits (account/order numbers), e-mail addresses, `/Users/<name>` -> `~`, long key-like tokens, `key=value` secrets
  and webhook URLs are masked. Message size is clipped to 1800 characters; `@everyone`/mentions are disabled.
* Identical messages within 10 minutes are sent once; Discord's 429 rate limit is honoured with one retry.
