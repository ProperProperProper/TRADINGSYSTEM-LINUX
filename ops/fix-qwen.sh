#!/bin/bash
set -euo pipefail
exec python3 /home/patrick/TradingSystem/ops/repair_qwen_host.py --repair-inference --install-training --restart-project
