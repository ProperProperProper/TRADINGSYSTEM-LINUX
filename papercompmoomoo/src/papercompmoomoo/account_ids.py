"""Per-install paper-account IDs kept outside the source repository."""
from __future__ import annotations

import json
from pathlib import Path


def account_id(bot: str) -> int:
    if bot not in {"gainer", "orb"}:
        raise ValueError(f"unknown paper bot: {bot}")
    candidates = [
        Path.cwd() / "data" / "account_ids.json",
        Path(__file__).resolve().parents[2] / "data" / "account_ids.json",
    ]
    path = next((candidate for candidate in candidates if candidate.exists()), candidates[0])
    try:
        values = json.loads(path.read_text())
        gainer, orb = int(values["gainer"]), int(values["orb"])
    except (OSError, ValueError, KeyError, TypeError) as exc:
        raise RuntimeError(f"paper account IDs are not configured in {path}") from exc
    if gainer <= 0 or orb <= 0 or gainer != orb:
        raise RuntimeError(f"Gainers and ORB must share one configured Competition account in {path}")
    return gainer


def paper_account_ids() -> set[int]:
    return {account_id("gainer"), account_id("orb")}
