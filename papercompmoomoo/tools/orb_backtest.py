"""Recorded-trade exit replay for the ORB bot using its own candidate ledger."""
from __future__ import annotations
import argparse
import json
from pathlib import Path
from research_replay import DATA, run


def main():
    parser = argparse.ArgumentParser(description="Replay recorded ORB entries with the live shared exit lines")
    parser.add_argument("--data", type=Path, default=DATA)
    parser.add_argument("--cost-bps", type=float, default=10.0)
    parser.add_argument("--output", type=Path, default=DATA / "orb_backtest.json")
    args = parser.parse_args()
    report = run("orb", args.data, args.cost_bps)
    result = report["bots"]["orb"]
    result.update({"strategy": "orb_live_recorded_entry_exit_replay", "method": report["method"],
                   "limitations": report["limitations"], "cost_bps_per_round_trip": report["cost_bps_per_round_trip"]})
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k not in ("trades", "candidate_records")}, indent=2))


if __name__ == "__main__":
    main()
