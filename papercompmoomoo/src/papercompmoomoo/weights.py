"""How much each agent's work counts (owner order 2026-10-21: "everything they do needs to be weighted properly").

An agent's weight is its track record, not its say-so: how often its calls turned into trades that made money, shrunk toward what an uninformed call achieves while there are few trades.
* a trade is a hit when it closed above zero after costs;
* the uninformed hit rate is ``BASE_WIN_RATE`` (about 40% across 230,000 simulated entries of this project's own history: more trades lose than win once costs are paid);
* the posterior hit rate is ``(BASE_WIN_RATE * STRENGTH + hits) / (STRENGTH + n)``: with no trades it IS the base rate, and the evidence takes over as trades accumulate;
* the weight is that posterior over twice the base rate, between ``MIN_WEIGHT`` and 1: the base rate weighs 0.5, a record twice as good as chance weighs 1, a record that is worse than chance weighs less.
Weights are used where an agent's output meets a deterministic alternative: the conditions an agent sets for a symbol are blended with the conditions its own history implies (``blend``), and every
message on the board carries its sender's weight so a reader can count it accordingly.
"""
from __future__ import annotations

BASE_WIN_RATE = 0.40
STRENGTH = 10
MIN_WEIGHT = 0.1


def weight(hits: int, n: int) -> float:
    posterior = (BASE_WIN_RATE * STRENGTH + hits) / (STRENGTH + max(0, n))
    return max(MIN_WEIGHT, min(1.0, posterior / (2 * BASE_WIN_RATE)))


def blend(agent: dict, fallback: dict, w: float) -> dict:
    """Each number is ``w`` of the agent's choice and ``1 - w`` of the fallback; a number only one side has is taken as it is."""
    out = {}
    for key in set(agent) | set(fallback):
        if key in agent and key in fallback:
            out[key] = round(w * agent[key] + (1 - w) * fallback[key], 2)
        else:
            out[key] = agent[key] if key in agent else fallback[key]
    return out


def from_journal(db, board_db=None) -> dict[str, dict]:
    """The planner's and the assessor's records from the closed trades of the journal (``scout_learnings``): the planner's trades are the plan's entries, the assessor's the rule entries it set conditions for."""
    def record(where: str) -> tuple[int, int]:
        n, hits = db.execute(f"SELECT count(*), coalesce(sum(pnl_pct > 0), 0) FROM scout_learnings WHERE {where}").fetchone()
        return int(n), int(hits)

    out = {}
    from . import conduct
    stopped = conduct.suspended_agents(board_db) if board_db is not None else set()  # suspensions live on the board's database
    for agent, where in (("planner", "source='plan'"), ("assessor", "source='rule' AND terms_json != '{}'")):
        n, hits = record(where)
        out[agent] = {"weight": 0.0 if agent in stopped else round(weight(hits, n), 3), "trades": n, "hits": hits, **({"suspended": True} if agent in stopped else {})}
    return out
