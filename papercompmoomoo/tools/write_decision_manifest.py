#!/usr/bin/env python3
"""Write provenance for an existing ThomasAi decision corpus."""
from __future__ import annotations

import hashlib
import json
import argparse
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "data" / "thomasai_decision"
BASE = Path.home() / ".omlx" / "models" / "Qwen3.5-9B-oQ4e-mtp"


def digest(path: Path) -> dict[str, object]:
    return {"bytes": path.stat().st_size, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="verify the existing manifest without rewriting it")
    args = parser.parse_args()
    files = {name: OUT.joinpath(f"{name}.jsonl") for name in ("train", "valid", "test")}
    counts = {name: sum(1 for _ in path.open(encoding="utf-8")) for name, path in files.items()}
    brain_ids = set()
    for path in files.values():
        for line in path.open(encoding="utf-8"):
            brain_ids.update(json.loads(line)["source"]["brain_ids"])
    base_files = {}
    for name in ("config.json", "model.safetensors.index.json", "tokenizer_config.json"):
        path = BASE / name
        if not path.is_file():
            raise SystemExit(f"missing Qwen base file: {path}")
        base_files[name] = digest(path)
    manifest = {
        "schema": 3,
        "model": "ThomasAi",
        "capability": "decision",
        "base_checkpoint": {"name": BASE.name, "path": str(BASE), "files": base_files},
        "source": "real closed 1-minute events from common1m.sqlite3",
        "splitting": "chronological session split: train 70%, validation 15%, test 15%",
        "counts": counts,
        "brain_ids": sorted(brain_ids),
        "prompt_contract": "signal-bar fields only; forward bars are evaluation-only",
        "label_contract": "Rust causal eligibility only; no synthetic or model-generated labels",
        "promotion": "Rust evaluation, held-out evidence and safety gates required; never auto-activate",
        "files": {name: digest(path) for name, path in files.items()},
    }
    manifest_path = OUT / "manifest.json"
    if args.check:
        try:
            existing = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            raise SystemExit(f"invalid decision manifest: {exc}") from exc
        if existing != manifest:
            raise SystemExit("decision manifest does not match the corpus or Qwen base")
        print("decision manifest: OK")
        return
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
