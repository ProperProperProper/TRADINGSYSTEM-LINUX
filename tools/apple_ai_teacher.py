#!/usr/bin/env python3
"""
Apple AI Teacher Bridge for ThomasAi.
Interfaces with macOS Apple Intelligence & Apple Silicon Neural Engine
to analyze trade performance and deposit reflective counterfactual memories.
"""

import os
import sys
import json
import sqlite3
import subprocess

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
BRAIN_DB = os.path.join(REPO_ROOT, "papercompmoomoo", "data", "brain.sqlite3")
JOURNAL_DB = os.path.join(REPO_ROOT, "papercompmoomoo", "data", "journal.sqlite3")
THOMAS_BIN = os.path.join(REPO_ROOT, "target", "release", "thomas")


def run_apple_teacher():
    print(f"[Apple AI Teacher] Initiating learning cycle across {JOURNAL_DB} -> {BRAIN_DB}...")
    if not os.path.exists(THOMAS_BIN):
        print(f"Building release binary...")
        subprocess.run(["cargo", "build", "--release"], cwd=REPO_ROOT, check=True)

    res = subprocess.run([THOMAS_BIN, "apple-teacher"], cwd=REPO_ROOT, capture_output=True, text=True)
    print(res.stdout.strip())
    if res.stderr:
        print(res.stderr.strip(), file=sys.stderr)

    print("[Apple AI Teacher] Triggering autonomous neurogenesis...")
    step_res = subprocess.run([THOMAS_BIN, "neural-step"], cwd=REPO_ROOT, capture_output=True, text=True)
    print(step_res.stdout.strip())


if __name__ == "__main__":
    run_apple_teacher()
