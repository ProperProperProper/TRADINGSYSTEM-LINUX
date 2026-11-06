#!/usr/bin/env python3
"""Apply the Rust promotion gate to a ThomasAi candidate and persist the verdict."""
from __future__ import annotations

import argparse
import json
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TRADING = ROOT.parent
THOMAS = TRADING / "target" / "release" / "thomas"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--candidate", type=Path, default=ROOT / "data" / "thomasai")
    parser.add_argument("--validation-id", type=int, required=True)
    args = parser.parse_args()
    candidate = args.candidate if args.candidate.is_absolute() else (ROOT / args.candidate)
    command = [str(THOMAS), "model-gate", "--candidate", str(candidate), "--validation-id", str(args.validation_id)]
    result = subprocess.run(command, cwd=TRADING, text=True, capture_output=True)
    output = (result.stdout + result.stderr).strip()
    if result.returncode == 0:
        recovery = "none"
    elif "candidate return" in output and "below base" in output:
        recovery = "collect_positive_causal_examples_then_retrain"
    else:
        recovery = "inspect_rust_gate_failure_before_retraining"
    verdict = {
        "checked_ms": int(time.time() * 1000),
        "candidate": str(args.candidate),
        "validation_id": args.validation_id,
        "status": "viable" if result.returncode == 0 else "rejected",
        "rust_exit": result.returncode,
        "rust_output": output,
        "recovery_action": recovery,
        "previous_candidate_preserved": result.returncode != 0,
        "activated": False,
    }
    candidate.mkdir(parents=True, exist_ok=True)
    (candidate / "viability.json").write_text(json.dumps(verdict, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(verdict, indent=2))
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
