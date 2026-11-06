# Resource safety (16 GB Mac)

## Why it exists
On 2026-10-13 the machine froze and rebooted (load average 46 at boot). The cause was the local language-model stack that has since been uninstalled
(an inference server writing 34 GB to its SSD cache in two hours, with training overlapping inference and a login stampede on top). Nothing of it
runs any more. What remains watches the Mac and the lab so a slow build-up is caught early.

## Monitoring and self-repair (all Rust: `src/ops.rs`, binary `qwen-ops`; installer `ops/resource_safety/install.sh`)
* `qwen-ops watch` (launchd every 5 min): load, free memory, swap, disk to `logs/resource.log`; alerts to `logs/alerts.log` (load 20, memory under 10 %,
  swap 1.5 GB, disk under 20 GB); then repairs: restarts the bot, dashboard, learning loop, trainer and brain when stopped (and re-registers a job whose
  re-signed binary launchd refuses, exit 78), and restarts the brain if it has been silent for 45 minutes.
* `qwen-ops watchdog` (every 10 min): free while healthy; when a problem persists (new alerts, swap, OpenD down, the brain silent for an hour) it starts ONE
  headless Claude session (at most once per 30 min) that finds the root cause and fixes it; never asks questions, never touches trading logic.
* `qwen-ops health`: services, memory, the trainer's and the brain's recent runs, the ML-for-trading verdicts, heal actions, crash reports.
* The deep-learning trainer (`com.qwenlab.trainer`, Nice 15) pauses itself when the load is above 1.5 per core, during US market hours uses two threads,
  and stops for the plain switch files `STOP`, `MAINTENANCE`, `trainer/OFF` and `brain/OFF`.
* `qwen-notify` (Discord, docs/NOTIFICATIONS.md) and `qwen-guard` (webhook-protection git hooks) are separate small binaries.
