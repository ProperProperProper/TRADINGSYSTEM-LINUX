"""Compatibility entrypoint for Gainer's recorded-trade research replay."""
from __future__ import annotations
import argparse
import json
from pathlib import Path
from research_replay import DATA, run


def main():
    parser = argparse.ArgumentParser(description="Replay recorded Gainer entries with the live shared exit lines")
    parser.add_argument("--db", type=Path, default=DATA / "common1m.sqlite3")
    parser.add_argument("--journal", type=Path, default=DATA / "journal.sqlite3")
    parser.add_argument("--cost-bps", type=float, default=10.0)
    parser.add_argument("--output", type=Path, default=DATA / "gainer_backtest.json")
    args = parser.parse_args()
    data = args.db.resolve().parent
    if args.journal.resolve() != (data / "journal.sqlite3").resolve():
        raise SystemExit("Gainer journal and candle store must be in the same bot data directory")
    report = run("gainer", data, args.cost_bps)
    result = report["bots"]["gainer"]
    result.update({"strategy": "gainer_live_recorded_entry_exit_replay", "method": report["method"],
                   "limitations": report["limitations"], "cost_bps_per_round_trip": report["cost_bps_per_round_trip"]})
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k not in ("trades", "candidate_records")}, indent=2))


if __name__ == "__main__":
    main()
