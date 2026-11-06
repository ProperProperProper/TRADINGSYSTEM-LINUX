#!/bin/bash
set -euo pipefail
units=(
  local.papercompmoomoo.supervisor.service
  local.papercompmoomoo.codex-exit.service
  local.papercompmoomoo.standby.service
  com.thomasai.portal.service
)
install -Dm644 /home/patrick/TradingSystem/ops/systemd/local.orb.premarket.timer \
  /home/patrick/.config/systemd/user/local.orb.premarket.timer
systemctl --user daemon-reload
# Codex alone makes live paper exit decisions. This restart never installs or starts Qwen.
cd /home/patrick/TradingSystem/papercompmoomoo
PYTHONPATH=src .venv/bin/python -m papercompmoomoo.install codex-exit
for unit in "${units[@]}"; do
  systemctl --user restart "$unit"
  systemctl --user is-active --quiet "$unit"
  echo "active $unit"
done
