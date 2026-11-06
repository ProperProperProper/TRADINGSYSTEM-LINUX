"""Read-only audit of the simulated account: ``PYTHONPATH=src .venv/bin/python -m papercompmoomoo.check``.

Prints equity, cash, buying power, positions and working orders with their age, and exits with 1 when something is wrong: a working order of this bot older than
``stale_order_seconds``, duplicate working orders, positions or orders larger than the per-position limit, or an account that cannot be read. Run it after every
code change and restart (the bot itself also audits and fixes the account at start); it changes nothing.
"""

from __future__ import annotations

import sys
from datetime import datetime, timezone

from futu import OpenSecTradeContext, RET_OK, TrdEnv, TrdMarket

from .bot import Settings
from .signals import orders_to_cancel


def audit(context, settings: Settings | None = None) -> tuple[list[str], list[str]]:
    """(report lines, problems) for the simulated account behind ``context``."""
    settings = settings or Settings()
    lines: list[str] = []
    problems: list[str] = []
    from .account import KIND, pick_account

    ret0, accounts = context.get_acc_list()
    acc_id = pick_account(accounts, KIND) if ret0 == RET_OK else None
    if acc_id is None:
        return lines, [f"no single {KIND} simulated US account found"]
    lines.append(f"account {acc_id} ({KIND})")
    ret, info = context.accinfo_query(trd_env=TrdEnv.SIMULATE, acc_id=acc_id, currency="USD", refresh_cache=True)
    ret2, held = context.position_list_query(trd_env=TrdEnv.SIMULATE, acc_id=acc_id, refresh_cache=True)
    ret3, orders = context.order_list_query(trd_env=TrdEnv.SIMULATE, acc_id=acc_id, refresh_cache=True)
    if RET_OK not in (ret, ret2, ret3) or ret != RET_OK or ret2 != RET_OK or ret3 != RET_OK:
        return lines, ["the simulated account cannot be read"]
    row = info.iloc[0]
    equity, cash, power = float(row["total_assets"]), float(row["us_cash"]), float(row["power"])
    lines.append(f"equity {equity:,.2f} | cash {cash:,.2f} | buying power {power:,.2f}")
    if cash < 0:
        problems.append("cash is negative: something is reserving money (a working order or a margin position)")
    limit = equity * settings.max_position_pct * 1.05
    for _, p in held.iterrows():
        value = float(p.get("market_val", 0.0))
        lines.append(f"position {p['code']} qty {int(p['qty'])} cost {float(p.get('cost_price', 0.0)):.4f} value {value:,.2f}")
        if value > limit:
            problems.append(f"position {p['code']} is {value:,.0f}, above the per-position limit {limit:,.0f}")
    working = orders[orders["order_status"].isin({"WAITING_SUBMIT", "SUBMITTING", "SUBMITTED", "FILLED_PART"})] if len(orders) else orders
    rows = [{"order_id": int(r["order_id"]), "code": str(r["code"]), "remark": str(r.get("remark", "")),
             "created": datetime.strptime(str(r["create_time"])[:19], "%Y-%m-%d %H:%M:%S")} for _, r in working.iterrows()]
    for _, r in working.iterrows():
        lines.append(f"working {r['trd_side']} {r['code']} qty {int(r['qty'])} since {r['create_time']} ({r.get('remark', '')})")
    for r in orders_to_cancel(rows, datetime.now(timezone.utc), settings.stale_order_seconds):
        problems.append(f"order {r['order_id']} for {r['code']} is stale or a duplicate")
    import json
    from pathlib import Path

    from .deploy import code_hash

    try:
        reported = json.loads(Path("data/status.json").read_text()).get("code_hash")
    except (OSError, ValueError):
        reported = None
    if reported != code_hash():
        problems.append("the running bot is not on the code on disk (restart it): " + ("it never reported its code" if reported is None else "hashes differ"))
    return lines, problems


def main() -> None:
    context = OpenSecTradeContext(filter_trdmarket=TrdMarket.US, host=Settings.host, port=Settings.port)
    try:
        lines, problems = audit(context)
    finally:
        context.close()
    print("\n".join(lines) or "(nothing read)")
    print("PROBLEMS:\n  " + "\n  ".join(problems) if problems else "account ok")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
