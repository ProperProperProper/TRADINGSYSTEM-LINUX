"""Single-owner policy for strategy exits from the two paper accounts.

Entries and broker reconciliation remain with each bot.  Strategy SELL
decisions belong exclusively to the Codex exit worker. This module is kept
small and dependency-free so every execution path can fail closed.
"""
from __future__ import annotations
from pathlib import Path
from .account_ids import paper_account_ids

CODEX_EXIT_REMARK = "CODEX_EXIT:"
PAPER_ACCOUNTS = paper_account_ids()


def strategy_exit_owner(account_id: int) -> str:
    if int(account_id) not in PAPER_ACCOUNTS:
        raise ValueError("exit ownership is defined only for the two paper accounts")
    return "codex"


def legacy_strategy_exit_allowed(account_id: int) -> bool:
    """Always false: there is no runtime flag that can re-enable old exits."""
    strategy_exit_owner(account_id)
    return False


def require_codex_submission(account_id: int, remark: str, *, simulated: bool) -> None:
    """Reject any strategy SELL not approved by Codex or not simulated."""
    strategy_exit_owner(account_id)
    if not simulated:
        raise PermissionError("Codex exits are paper-only")
    if not str(remark).startswith(CODEX_EXIT_REMARK):
        raise PermissionError("strategy exits are exclusively owned by Codex")
