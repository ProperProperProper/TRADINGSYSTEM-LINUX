"""Alpha Strategy Implementations:
1. Strategy 1: Rank 1 Alpha Engine (100% allocation on Rank 1 momentum leader with per-symbol TP/SL)
2. Strategy 2: High-Conviction Core + Tier-1 Booster (Rank 1 + Rank 2 with Score >= 0.20, abstaining on ranks 3-15)
3. Strategy 3: Dynamic Conviction Portfolio Sizing (Rank 1 at 60%, Rank 2 at 25%, Rank 3 at 15%)
4. Strategy 4: Two-Leg Staged Scale-Out (+4% & +8% PT)
"""
from __future__ import annotations
from dataclasses import dataclass
from enum import Enum
from typing import Sequence
from .symbol_optimizer import get_symbol_optimal_bracket

class StrategyMode(str, Enum):
    RANK_1_ALPHA_ENGINE = "rank_1_alpha_engine"
    CORE_TIER1_BOOSTER = "core_tier1_booster"
    DYNAMIC_CONVICTION_PORTFOLIO = "dynamic_conviction_portfolio"
    STAGED_SCALE_OUT = "staged_scale_out"
    REGIME_ADAPTIVE = "regime_adaptive"


def detect_market_regime(common_db_path: Path | None = None) -> tuple[str, StrategyMode]:
    """Classify the current market regime from recent QQQ / broad momentum evidence:
    - 'strong_momentum_bull': Top rank momentum carries strong follow-through -> RANK_1_ALPHA_ENGINE
    - 'rotational_or_choppy': Mean-reverting intraday swings -> STAGED_SCALE_OUT
    """
    try:
        db_file = common_db_path or (ROOT / "data" / "common1m.sqlite3")
        if not db_file.exists():
            return "rotational_or_choppy", StrategyMode.STAGED_SCALE_OUT
        with sqlite3.connect(f"file:{db_file}?mode=ro", uri=True) as conn:
            # Query recent bars of QQQ or top ETF proxy
            df = pd.read_sql_query("""
                SELECT ts, close, open
                FROM bars
                WHERE symbol IN ('QQQ', 'SPY')
                ORDER BY ts DESC
                LIMIT 50
            """, conn)
            if len(df) >= 20:
                ret = df.iloc[0]['close'] / df.iloc[-1]['close'] - 1.0
                if ret > 0.015:
                    return "strong_momentum_bull", StrategyMode.RANK_1_ALPHA_ENGINE
                elif ret < -0.015:
                    return "risk_off_defensive", StrategyMode.CORE_TIER1_BOOSTER
    except Exception:
        pass
    return "rotational_or_choppy", StrategyMode.STAGED_SCALE_OUT

@dataclass(frozen=True)
class TargetCandidate:
    moomoo_symbol: str
    yahoo_symbol: str
    rank: int
    score: float
    weight: float
    take_profit_pct: float
    stop_loss_pct: float
    staged_scale: tuple[float, float] | None
    strategy_mode: StrategyMode

def select_alpha_candidates(
    candidates: Sequence[dict],
    mode: StrategyMode = StrategyMode.DYNAMIC_CONVICTION_PORTFOLIO
) -> list[TargetCandidate]:
    """Filter, size, and optimize bracket parameters per symbol according to the active strategy."""
    if not candidates:
        return []

    sorted_cands = sorted(candidates, key=lambda c: c.get("rank", 999))
    targets = []

    if mode == StrategyMode.RANK_1_ALPHA_ENGINE:
        for c in sorted_cands:
            if c.get("rank") == 1:
                sym = c.get("yahoo_symbol", "")
                tp, sl, staged = get_symbol_optimal_bracket(sym)
                targets.append(TargetCandidate(
                    moomoo_symbol=c.get("moomoo_symbol", ""),
                    yahoo_symbol=sym,
                    rank=1,
                    score=float(c.get("score", 0.0)),
                    weight=1.0,
                    take_profit_pct=tp,
                    stop_loss_pct=sl,
                    staged_scale=None,
                    strategy_mode=mode
                ))
                break

    elif mode == StrategyMode.STAGED_SCALE_OUT:
        for c in sorted_cands:
            if c.get("rank") == 1:
                sym = c.get("yahoo_symbol", "")
                tp, sl, staged = get_symbol_optimal_bracket(sym)
                targets.append(TargetCandidate(
                    moomoo_symbol=c.get("moomoo_symbol", ""),
                    yahoo_symbol=sym,
                    rank=1,
                    score=float(c.get("score", 0.0)),
                    weight=1.0,
                    take_profit_pct=tp,
                    stop_loss_pct=sl,
                    staged_scale=staged or (0.04, 0.08),
                    strategy_mode=mode
                ))
                break

    elif mode == StrategyMode.CORE_TIER1_BOOSTER:
        selected = []
        for c in sorted_cands:
            r = c.get("rank", 999)
            s = float(c.get("score", 0.0))
            if r == 1:
                selected.append((c, 0.65))
            elif r == 2 and s >= 0.20:
                selected.append((c, 0.35))
        
        tot_w = sum(w for _, w in selected) if selected else 1.0
        for c, w in selected:
            sym = c.get("yahoo_symbol", "")
            tp, sl, staged = get_symbol_optimal_bracket(sym)
            targets.append(TargetCandidate(
                moomoo_symbol=c.get("moomoo_symbol", ""),
                yahoo_symbol=sym,
                rank=c.get("rank", 999),
                score=float(c.get("score", 0.0)),
                weight=round(w / tot_w, 4),
                take_profit_pct=tp,
                stop_loss_pct=sl,
                staged_scale=None,
                strategy_mode=mode
            ))

    elif mode == StrategyMode.DYNAMIC_CONVICTION_PORTFOLIO:
        selected = []
        for c in sorted_cands:
            r = c.get("rank", 999)
            s = float(c.get("score", 0.0))
            if r == 1:
                selected.append((c, 0.60))
            elif r == 2 and s >= 0.20:
                selected.append((c, 0.25))
            elif r == 3 and s >= 0.25:
                selected.append((c, 0.15))
        
        if not selected:
            for c in sorted_cands:
                if c.get("rank") == 1:
                    selected.append((c, 1.0))
                    break

        tot_w = sum(w for _, w in selected) if selected else 1.0
        for c, w in selected:
            sym = c.get("yahoo_symbol", "")
            tp, sl, staged = get_symbol_optimal_bracket(sym)
            targets.append(TargetCandidate(
                moomoo_symbol=c.get("moomoo_symbol", ""),
                yahoo_symbol=sym,
                rank=c.get("rank", 999),
                score=float(c.get("score", 0.0)),
                weight=round(w / tot_w, 4),
                take_profit_pct=tp,
                stop_loss_pct=sl,
                staged_scale=staged,
                strategy_mode=mode
            ))

    return targets
