#!/usr/bin/env python3
"""Run a ThomasAi adapter on frozen momentum candidates for Rust evaluation.

This is an exclusive candidate probe, not a live service.  It never submits
orders and never sends forward bars to the model.  Rust evaluates the emitted
envelopes before any candidate can be admitted.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

from tools import qwen_decisions


MAX_EVENTS = 40
MAX_MODEL_DISK_BYTES = 10 * 1024**3


def disk_bytes(path: Path) -> int:
    return sum(item.stat().st_size for item in path.rglob("*") if item.is_file())


def ensure_model_disk_budget(model: Path, adapter: Path) -> None:
    total = disk_bytes(model) + disk_bytes(adapter)
    if total > MAX_MODEL_DISK_BYTES:
        raise SystemExit(
            f"refusing candidate: on-disk base plus adapter is {total / 1024**3:.2f} GiB; "
            "limit is 10.00 GiB"
        )


def adapter_version(adapter: Path) -> str:
    digest = hashlib.sha256()
    weights = sorted(adapter.glob("*.safetensors"))
    if not weights:
        raise SystemExit(f"adapter has no SafeTensors weights: {adapter}")
    for path in weights:
        digest.update(path.name.encode())
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
    return f"ThomasAi:{digest.hexdigest()[:16]}"


def prompt_for(event: dict[str, object]) -> str:
    observed = {
        "session": event["session"],
        "rank": event["rank"],
        "score": event["score"],
        "observations": event["observations"],
        "lookback_sessions": 20,
        "data_cutoff": event["data_cutoff"],
    }
    return (
        "Return ONLY a schema-1 JSON object with schema, decision, confidence, "
        "and reason. For this decision capability, decision must be enter or "
        "abstain; an enter decision requires confidence greater than 0. "
        "confidence must be a number from 0 to 1. Keep reason under 80 characters. "
        "Do not invent facts.\n"
        f"FROZEN_MOMENTUM_CANDIDATE {json.dumps(observed, separators=(',', ':'))}\n"
        "OUTPUT_JSON schema decision confidence reason"
    )


def generate_envelope(model: object, tokenizer: object, prompt: str) -> dict[str, object]:
    """Generate at most two bounded attempts; never repair model JSON."""
    from mlx_lm import generate as mlx_generate

    prompts = (
        prompt,
        prompt + "\nRETRY: output exactly one compact JSON object now; use abstain if unsure.",
    )
    last_error: Exception | None = None
    for attempt, current in enumerate(prompts):
        try:
            raw = mlx_generate(model, tokenizer, prompt=current, max_tokens=512 if attempt == 0 else 128)
            value = qwen_decisions.parse_decision_object(raw)
            decision = value.get("decision")
            confidence = value.get("confidence")
            reason = value.get("reason")
            if decision not in {"enter", "abstain"}:
                raise ValueError("adapter returned an invalid decision")
            if isinstance(confidence, bool) or not isinstance(confidence, (int, float)) or not 0 <= confidence <= 1:
                raise ValueError("adapter returned invalid confidence")
            if decision != "abstain" and confidence <= 0:
                raise ValueError("non-abstain decisions require positive confidence")
            if not isinstance(reason, str) or not reason.strip() or len(reason.strip()) > 80:
                raise ValueError("adapter returned invalid reason")
            return value
        except (json.JSONDecodeError, TypeError, ValueError) as error:
            last_error = error
    raise ValueError(f"adapter failed schema-1 recovery: {last_error}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--events", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--adapter", type=Path, required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--limit", type=int, default=1)
    args = parser.parse_args()
    if not 1 <= args.limit <= MAX_EVENTS:
        raise SystemExit(f"--limit must be between 1 and {MAX_EVENTS}")
    ensure_model_disk_budget(args.model, args.adapter)
    model_version = adapter_version(args.adapter)
    try:
        from mlx_lm import generate, load
    except ModuleNotFoundError as error:
        raise SystemExit("the candidate runner requires the MLX training environment") from error
    events = [json.loads(line) for line in args.events.read_text().splitlines() if line.strip()]
    if not events:
        raise SystemExit("event stream is empty")
    model, tokenizer = load(str(args.model), adapter_path=str(args.adapter))
    written = 0
    with args.output.open("w", encoding="utf-8") as target:
        for event in events:
            if written >= args.limit:
                break
            prompt = tokenizer.apply_chat_template(
                [{"role": "user", "content": prompt_for(event)}],
                tokenize=False,
                add_generation_prompt=True,
                enable_thinking=False,
            )
            response = generate_envelope(model, tokenizer, prompt)
            decision = response.get("decision")
            confidence = response.get("confidence")
            reason = response.get("reason")
            metadata = qwen_decisions.authoritative_metadata(event, str(event["data_cutoff"]))
            target.write(json.dumps({
                **metadata,
                "schema": 1,
                "decision": decision,
                "confidence": float(confidence),
                "reason": reason.strip(),
                "model_version": model_version,
            }, separators=(",", ":")) + "\n")
            written += 1
    print(json.dumps({"written": written, "output": str(args.output)}))


if __name__ == "__main__":
    main()
