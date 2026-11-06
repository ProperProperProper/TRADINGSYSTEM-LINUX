"""Runtime location invariants shared by every long-running entry point."""

from __future__ import annotations

import os
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[2]


def enter_project() -> Path:
    """Run relative data/config paths from the checkout, regardless of the caller's cwd."""
    os.chdir(PROJECT_ROOT)
    return PROJECT_ROOT
