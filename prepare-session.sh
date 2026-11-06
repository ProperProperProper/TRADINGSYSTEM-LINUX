#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="/home/patrick/TradingSystem"
PAPER="$ROOT/papercompmoomoo"
fail() { printf 'NOT READY: %s\n' "$*" >&2; exit 1; }

cd "$ROOT"
command -v cargo >/dev/null || fail "cargo is not on PATH"
[[ -x "$PAPER/.venv/bin/python" ]] || fail "project Python environment is missing"

printf '%s\n' 'Building current release binary...'
CARGO_BUILD_JOBS=5 cargo build --release -p thomas

printf '%s\n' 'Installing/restarting local inference and live bot services...'
cd "$PAPER"
PYTHONPATH=src:. .venv/bin/python -m papercompmoomoo.install all

printf '%s\n' 'Waiting for local Qwen...'
for _ in $(seq 1 120); do
  if curl -fsS --max-time 2 http://127.0.0.1:8000/v1/models >/dev/null; then break; fi
  sleep 2
done
curl -fsS --max-time 3 http://127.0.0.1:8000/v1/models >/dev/null || fail "Qwen did not start on port 8000"

printf '%s\n' 'Waiting for ThomasAi coordinator heartbeat...'
for _ in $(seq 1 60); do
  if PYTHONPATH=src:. .venv/bin/python -c 'import json,time; from pathlib import Path; p=Path("data/agents_status.json"); s=json.loads(p.read_text()) if p.exists() else {}; raise SystemExit(0 if s.get("backends",{}).get("local",{}).get("status")=="ok" and time.time()*1000-s.get("updated_ms",0)<120000 else 1)' 2>/dev/null; then break; fi
  sleep 2
done
PYTHONPATH=src:. .venv/bin/python -c 'import json,time; from pathlib import Path; s=json.loads(Path("data/agents_status.json").read_text()); assert s.get("backends",{}).get("local",{}).get("status")=="ok" and time.time()*1000-s.get("updated_ms",0)<120000' || fail "coordinator has not confirmed Qwen"

systemctl --user --no-pager is-active \
  com.thomasai.portal.service \
  local.qwen.service local.papercompmoomoo.standby.service \
  local.papercompmoomoo.supervisor.service local.papercompmoomoo.codex-exit.service \
  local.orb.account.timer local.orb.paper.timer local.orb.premarket.timer \
  || fail "a required user service is not active"

PYTHONPATH="$PAPER/src:$PAPER" "$PAPER/.venv/bin/python" - <<'PY'
import json, sqlite3, time
from datetime import datetime
from pathlib import Path
from papercompmoomoo.deploy import code_hash

data = Path("/home/patrick/TradingSystem/papercompmoomoo/data")
ids = json.loads((data / "account_ids.json").read_text())
if int(ids.get("gainer", 0)) <= 0 or ids.get("gainer") != ids.get("orb"):
    raise SystemExit("NOT READY: both bots must share the configured Competition paper account")
status = json.loads((data / "status.json").read_text())
stamp = status.get("updated_at")
if not stamp or time.time() - datetime.fromisoformat(stamp).timestamp() > 180:
    raise SystemExit("NOT READY: Gainers account/runtime status is missing or older than 180 seconds")
if status.get("account_kind") != "COMPETITION":
    raise SystemExit("NOT READY: Gainers is not pinned to the Competition paper account")
if status.get("code_hash") != code_hash():
    raise SystemExit("NOT READY: Gainers process has not reported the current source hash")
audit = json.loads((data / "audit.json").read_text())
if audit.get("errors", 0) or max(0, audit.get("warnings", 0)-audit.get("fixed", 0)):
    raise SystemExit(f"NOT READY: bot audit reports findings: {audit}")
health = json.loads((data / "candle_health.json").read_text())
if health.get("status") != "ok" or time.time() - health.get("checked_ms", 0)/1000 > 180:
    raise SystemExit("NOT READY: current Moomoo candle health is missing, stale, or unhealthy")
orb = json.loads((data / "orb_account.json").read_text())
if orb.get("account_kind") != "COMPETITION" or time.time() - orb.get("updated_ms", 0)/1000 > 90:
    raise SystemExit("NOT READY: ORB Competition account snapshot is missing or stale")
db = sqlite3.connect(f"file:{data / 'common1m.sqlite3'}?mode=ro", uri=True)
if not db.execute("select 1 from sqlite_master where type='table' and name='bars'").fetchone():
    raise SystemExit("NOT READY: one-minute bars table is missing")
count, symbols = db.execute("select count(*), count(distinct symbol) from bars").fetchone()
print(f"One-minute store: {count} bars across {symbols} symbols")
if count == 0:
    print("No bars stored yet; live candidate bars populate during the US session.")
PY

printf '\n%s\n' 'Current code and required services are active. Check minute-bar population during session open.'
