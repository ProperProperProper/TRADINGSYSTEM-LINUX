"""The bot's own auditor: it looks at everything the bot depends on, repairs what is safe to repair, and reports the rest. No person or outside tool is needed.

``evaluate`` is a pure function from facts to findings (so every rule is tested); ``MomentumPaperBot.run_audit`` gathers the facts, applies the repairs the findings
name, and writes ``data/audit.json`` for the dashboard. It runs at every start and every ``audit_seconds`` after that.

Checks: the competition account is the one traded and is readable; this bot's working orders are neither stale nor duplicated; what the account holds is what the
bot tracks; exposure and cash are sane; the candle store holds no junk; the running code is the
code on disk (the bot restarts itself, through the test gate, as soon as no order is working; open positions are adopted again). Repairs: cancel orders, adopt or drop positions, clean the
store (done while gathering the facts), restart. Price-level exits and the 15:00 ET flatten are owned by the exit worker.
"""

from __future__ import annotations

from dataclasses import dataclass

@dataclass(frozen=True)
class Finding:
    level: str  # "warn" or "error"
    area: str
    text: str
    fix: str | None = None  # the repair the bot applies: cancel_orders, adopt_positions, exit:<symbol>, restart


def _store_and_code(facts: dict) -> list[Finding]:
    out: list[Finding] = []
    junk = {k: v for k, v in (facts.get("store_junk") or {}).items() if v}
    if junk:
        out.append(Finding("warn", "candles", f"junk was found in the candle store and removed: {junk}"))
    if facts.get("code_changed"):
        ready = facts.get("flat") or facts.get("no_working")  # positions do not block it (owner 2026-10-22): a restart adopts what the account holds, keeps each one's high and armed state and resumes the locked plan; only a working order waits
        out.append(Finding("warn", "code", "the running code differs from the files on disk" + ("" if ready else " (restarting once no order is working)"),
                           "restart" if ready else None))
    return out


def evaluate(facts: dict) -> list[Finding]:
    """Findings for the facts of one look. ``facts`` keys: now, acc_id, account (snapshot or None; ``held`` rows carry ``value``), tracked (symbol -> position with
    ``opened``), stale_orders, per_position_limit, store_junk (counts removed), code_changed, flat,
    session_open."""
    if facts.get("acc_id") is None:
        return [Finding("error", "account", "no single COMPETITION simulated US account is available: nothing is traded")] + _store_and_code(facts)
    account = facts.get("account")
    if account is None:
        return [Finding("error", "account", "the competition account cannot be read: nothing is traded blind")] + _store_and_code(facts)
    out: list[Finding] = []
    held = {r["code"]: r for r in account["held"]}
    stale = facts.get("stale_orders", [])
    if stale:
        out.append(Finding("warn", "orders", f"{len(stale)} of this bot's orders are stale or duplicated ({', '.join(sorted({o['code'] for o in stale}))})", "cancel_orders"))
    untracked, vanished = sorted(set(held) - set(facts["tracked"])), sorted(set(facts["tracked"]) - set(held))
    if untracked or vanished:
        out.append(Finding("warn", "positions", f"the account holds {untracked or 'nothing'} the bot did not track; the bot tracks {vanished or 'nothing'} the account no longer holds",
                           "adopt_positions"))
    limit = facts.get("per_position_limit") or 0.0
    for symbol, row in held.items():
        if limit and row.get("value", 0.0) > limit:
            out.append(Finding("error", "exposure", f"{symbol} is worth {row['value']:,.0f}, above the per-position limit {limit:,.0f}"))
    if account["cash"] < 0 and account.get("power", 1.0) <= 0:
        out.append(Finding("warn", "account", f"cash is negative ({account['cash']:,.0f}) and buying power is exhausted ({account.get('power', 0.0):,.0f})"))
    return out + _store_and_code(facts)
