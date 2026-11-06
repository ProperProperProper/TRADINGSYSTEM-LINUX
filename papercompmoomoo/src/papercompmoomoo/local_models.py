"""No unused model sits on the disk (owner order 2026-10-21).

ONE model at a time (owner order 2026-10-21): the local model server finds models in ``~/.omlx/models`` and exactly one is kept, the one named in ``data/llm.json`` (``omlx_model``). While a replacement is being
downloaded it is protected (it is not a model yet), and the moment it becomes the active one the old one is deleted. Every other model directory is deleted, and so are leftovers of interrupted downloads.
The server's prompt cache (``~/.omlx/cache``) is not a model but is kept small too (``trim_cache``). A directory that is still being downloaded (a ``.incomplete`` file inside, or changed in the last
``DOWNLOAD_GRACE_S`` seconds) is never touched, so the model that is about to take over is not removed under the download. ``tidy`` reports what it would remove unless ``apply`` is set.
"""
from __future__ import annotations

import json
import shutil
import time
from pathlib import Path

MODELS_DIR = Path.home() / ".omlx" / "models"
CACHE_DIR = Path.home() / ".omlx" / "cache"
CACHE_MAX_BYTES = 1_000_000_000
DOWNLOAD_GRACE_S = 2 * 3600
TRAINING_MARKERS = {
    "adapter_config.json", "adapter_model.safetensors", "training_args.json",
    "trainer_state.json", "checkpoint", "training_lineage.sqlite3",
}


def size_bytes(path: Path) -> int:
    return sum(f.stat().st_size for f in path.rglob("*") if f.is_file())


def downloading(path: Path, now: float) -> bool:
    if any(path.rglob("*.incomplete")):
        return True
    newest = max((f.stat().st_mtime for f in path.rglob("*") if f.is_file()), default=0.0)
    return now - newest < DOWNLOAD_GRACE_S


def training_artifact(path: Path) -> bool:
    """Training/checkpoint/adapter trees are never cache-cleaner targets."""
    lowered={part.lower() for part in path.parts}
    if any(any(word in part for word in ('adapter','lora','checkpoint','training','corpus')) for part in lowered):
        return True
    return any(item.name.lower() in TRAINING_MARKERS for item in path.rglob('*') if item.is_file())


def keep_list(active: str) -> set[str]:
    """The one model that stays."""
    return {active}


def trim_cache(cache_dir: Path = CACHE_DIR, max_bytes: int = CACHE_MAX_BYTES, apply: bool = False) -> int:
    """Removes the oldest prompt-cache files until the folder is under ``max_bytes``; returns the bytes removed (or that would be)."""
    if not cache_dir.is_dir():
        return 0
    files = sorted((f for f in cache_dir.rglob("*") if f.is_file()), key=lambda f: f.stat().st_mtime)
    total = sum(f.stat().st_size for f in files)
    removed = 0
    for f in files:
        if total - removed <= max_bytes:
            break
        removed += f.stat().st_size
        if apply:
            f.unlink(missing_ok=True)
    return removed


def tidy(models_dir: Path, keep: set[str], apply: bool = False, now: float | None = None) -> list[dict]:
    """The model directories that are not kept (and not being downloaded), deleted when ``apply``; returns what was (or would be) removed."""
    now = now if now is not None else time.time()
    out = []
    if not models_dir.is_dir():
        return out
    for path in sorted(p for p in models_dir.iterdir() if p.is_dir()):
        if path.name in keep or downloading(path, now) or training_artifact(path):
            continue
        size = size_bytes(path)
        if apply:
            shutil.rmtree(path, ignore_errors=True)
        out.append({"model": path.name, "bytes": size, "removed": apply})
    return out


if __name__ == "__main__":
    import sys
    from . import llm
    apply = "--apply" in sys.argv
    result = tidy(MODELS_DIR, keep_list(llm.load_config()["omlx_model"]), apply=apply)
    freed = trim_cache(apply=apply)
    if freed:
        print(("trimmed" if apply else "would trim"), "prompt cache", round(freed / 1e9, 2), "GB")
    for r in result:
        print(("removed" if r["removed"] else "would remove"), r["model"], round(r["bytes"] / 1e9, 2), "GB")
    if not result:
        print("nothing unused")
