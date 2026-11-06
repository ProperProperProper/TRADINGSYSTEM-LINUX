"""Refresh the shared Competition account snapshot for the Rust ORB runner."""
import json
from pathlib import Path
from futu import OpenSecTradeContext, RET_OK, TrdEnv, TrdMarket
import sys
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))
sys.path.insert(0, str(ROOT / "tools"))
from papercompmoomoo.account import pick_account
from papercompmoomoo.account_ids import account_id

OUT = Path("data/orb_account.json")
ctx = OpenSecTradeContext(host="127.0.0.1", port=11111, filter_trdmarket=TrdMarket.US)
try:
    ret, accounts = ctx.get_acc_list()
    acc = pick_account(accounts, "COMPETITION") if ret == RET_OK else None
    if acc is None or acc != account_id("orb"):
        raise SystemExit("no Competition US simulated account exposed by OpenD")
    ret, info = ctx.accinfo_query(trd_env=TrdEnv.SIMULATE, acc_id=acc, currency="USD", refresh_cache=True)
    if ret != RET_OK:
        raise SystemExit("OpenD refused the Competition US account snapshot")
    row = info.iloc[0]
    payload = {"account_id": acc, "account_kind": "COMPETITION", "equity": float(row["total_assets"]), "cash": float(row["us_cash"]), "buying_power": float(row["power"]), "updated_ms": __import__("time").time_ns() // 1_000_000}
    OUT.write_text(json.dumps(payload, indent=2) + "\n")
    from orb_ledger import snapshot
    snapshot(payload)
    print(json.dumps(payload))
finally:
    ctx.close()
