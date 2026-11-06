#!/usr/bin/env python3
"""Ask the configured Claude/Codex/Gemini teacher for bounded momentum envelopes.

This adapter never invents market data or submits orders.  Rust remains the
only evaluator; malformed model responses are rejected and omitted.
"""
from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

from papercompmoomoo import brain, llm

ADAPTER_TIMEOUT_SECONDS = 10
MAX_DECISIONS_PER_RUN = 100


def parse_decision_object(text: str) -> dict[str, object]:
    """Parse only a JSON object, allowing known presentation wrappers.

    Provider formatting is not evidence.  Thinking tags and a single fenced
    JSON block are presentation wrappers; any other prose is rejected so a
    plausible-looking explanation can never become a trading decision.
    """
    cleaned = text.strip()
    if "<think>" in cleaned:
        cleaned = re.sub(r"<think>.*?</think>", "", cleaned, flags=re.DOTALL).strip()
    if cleaned.startswith("```json") and cleaned.endswith("```"):
        cleaned = cleaned[7:-3].strip()
    elif cleaned.startswith("```") and cleaned.endswith("```"):
        cleaned = cleaned[3:-3].strip()
    value = json.loads(cleaned)
    if not isinstance(value, dict):
        raise ValueError("model response is not a JSON object")
    return value


def authoritative_metadata(event: dict[str, object], data_cutoff: str) -> dict[str, object]:
    db = brain.connect()
    try:
        row = db.execute(
            "SELECT id FROM memories WHERE status='active' AND strategy_version IS NOT NULL "
            "AND trim(strategy_version)<>'' ORDER BY id LIMIT 1"
        ).fetchone()
    finally:
        db.close()
    if row is None:
        raise RuntimeError("no active versioned brain evidence exists")
    return {
        "symbol": dict(event["symbol"]),
        "signal_ts_ms": int(event["signal_ts_ms"]),
        "brain_ids": [int(row[0])],
        "board_ids": [],
        "data_cutoff": data_cutoff,
        "strategy_version": str(event["strategy"]),
        "model_version": "",
    }


def ask(event: dict[str, object], config: dict[str, object], data_cutoff: str) -> dict[str, object]:
    metadata = authoritative_metadata(event, data_cutoff)
    # Forward bars are reserved for causal replay. Sending them to a teacher
    # would leak the outcome being evaluated into the model decision.
    observed = {"session": event["session"], "rank": event["rank"], "score": event["score"],
                "observations": event["observations"], "lookback_sessions": 20,
                "data_cutoff": event["data_cutoff"]}
    prompt = (
        "Return ONLY JSON with decision, confidence, and reason. decision must be "
        "enter or abstain; confidence must be a number from 0 to 1. "
        "Use abstain when this frozen momentum candidate is insufficient. Keep reason under 80 characters. "
        "Do not invent facts. "
        f"FROZEN_MOMENTUM_CANDIDATE {json.dumps(observed, separators=(',', ':'))} OUTPUT_JSON decision confidence reason"
    )
    result = llm.complete(prompt, order=["claude", "codex", "gemini"], expect="json", config=config)
    model = parse_decision_object(result.text)
    decision = model.get("decision")
    if decision not in {"enter", "abstain"}:
        raise ValueError("teacher returned an invalid decision")
    confidence = model.get("confidence")
    if isinstance(confidence, bool) or not isinstance(confidence, (int, float)) or not 0 <= confidence <= 1:
        raise ValueError("teacher returned invalid confidence")
    if decision != "abstain" and confidence <= 0:
        raise ValueError("non-abstain decisions require positive confidence")
    reason = model.get("reason")
    if not isinstance(reason, str) or not reason.strip():
        raise ValueError("teacher returned no reason")
    # The managed oMLX HTTP server exposes the base checkpoint only.  A LoRA
    # adapter is not silently implied here; candidate inference must use an
    # explicit adapter-capable runner and its own provenance label.
    metadata["model_version"] = f"{result.backend}:{config.get('omlx_model', 'unknown')}" if result.backend == "omlx" else result.backend
    return {
        **metadata,
        "schema": 1,
        "decision": decision,
        "confidence": float(confidence),
        "reason": reason.strip(),
        "model_version": metadata["model_version"],
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--events", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--limit", type=int, default=1)
    args = parser.parse_args()
    if not 1 <= args.limit <= MAX_DECISIONS_PER_RUN:
        raise SystemExit(f"--limit must be between 1 and {MAX_DECISIONS_PER_RUN}")
    config = llm.load_config()
    config["omlx_max_tokens"] = min(int(config.get("omlx_max_tokens", 500)), 160)
    # The shared gateway allows long interactive calls; this unattended
    # decision adapter must hand off promptly when local inference is over the
    # machine's memory/time budget.
    llm.TIMEOUT = ADAPTER_TIMEOUT_SECONDS
    written = 0
    rejected = 0
    with args.events.open(encoding="utf-8") as source:
        all_events = [json.loads(line) for line in source if line.strip()]
    if not all_events:
        raise SystemExit("event stream is empty")
    newest_session = max(str(event["session"]) for event in all_events)
    all_events = [event for event in all_events if str(event["session"]) == newest_session]
    with args.output.open("w", encoding="utf-8") as target:
        for event in all_events:
            if written >= args.limit:
                break
            try:
                envelope = ask(event, config, str(event["data_cutoff"]))
            except (json.JSONDecodeError, KeyError, TypeError, ValueError, RuntimeError, llm.Failed) as error:
                rejected += 1
                print(f"rejected event: {type(error).__name__}: {error}")
                continue
            target.write(json.dumps(envelope, separators=(",", ":")) + "\n")
            written += 1
    print(json.dumps({"written": written, "rejected": rejected, "output": str(args.output)}))
    if written == 0:
        raise SystemExit("no valid model decisions were produced")


if __name__ == "__main__":
    main()
