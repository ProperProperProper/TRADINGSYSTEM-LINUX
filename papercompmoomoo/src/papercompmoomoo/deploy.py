"""A fingerprint of this project's source files, so "is the running bot on the latest code?" has an exact answer.

The bot writes ``code_hash()`` into ``data/status.json`` when it starts; ``python -m papercompmoomoo.check`` and TradingSystem's ``thomas doctor`` compare it with
the fingerprint of the files on disk now. The algorithm is the same as ``thomas_core::source_hash``: SHA-256 over, for every ``.py`` file under ``src/papercompmoomoo``
sorted by its path relative to the project folder (``/`` separators), the path, a NUL byte, the file's bytes and a NUL byte.
"""

from __future__ import annotations

import hashlib
from pathlib import Path

SKIP = {"target", ".git", "__pycache__", ".venv", "data", "node_modules"}


def code_hash(project: Path | None = None) -> str:
    project = project or Path.cwd()
    files = []
    for path in (project / "src" / "papercompmoomoo").rglob("*.py"):
        relative = path.relative_to(project)
        if not (SKIP & set(relative.parts)) and not any(part.endswith(".egg-info") for part in relative.parts):
            files.append((relative.as_posix(), path))
    digest = hashlib.sha256()
    for relative, path in sorted(files):
        digest.update(relative.encode() + b"\0" + path.read_bytes() + b"\0")
    return digest.hexdigest()
