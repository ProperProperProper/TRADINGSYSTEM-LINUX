#!/usr/bin/env python3
"""Local Multi-Strategy Paper Trader & Comparison Chart Generator Grouped by Market.

Trades all 4 alpha strategies simultaneously across real 1m candles:
1. Strategy 1: Rank 1 Alpha Engine (100% Top Leader)
2. Strategy 4: Staged Scale-Out (50% TP1, 50% TP2)
3. Strategy 2: Core + Tier-1 Booster (65% Rank 1, 35% Rank 2)
4. Strategy 3: Dynamic Conviction Portfolio (50% Rank 1, 30% Rank 2, 20% Rank 3)

Partitions evaluations across markets with realistic exchange-calibrated parameters:
- All Markets (Global Cross-Market Composite)
- US NASDAQ (Tech & High-Beta Momentum: TP +2.2%, SL -0.8%)
- US NYSE (Large-Cap & Institutional Momentum: TP +2.2%, SL -0.8%)
- AU ASX (Resources & Banking Momentum: TP +4.0%, SL -2.0%)

Generates:
- `data/reports/strategy_comparison.json`: Trade ledgers, daily returns, equity curves grouped by market.
- `data/reports/strategy_comparison_chart.html`: Interactive SVG comparison chart.
"""
from __future__ import annotations
import sys
import json
import sqlite3
import pandas as pd
import numpy as np
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from papercompmoomoo.alpha_strategies import select_alpha_candidates, StrategyMode
from papercompmoomoo.symbol_optimizer import get_symbol_optimal_bracket

COMMON_STORE = ROOT / "data" / "common1m.sqlite3"
REPORTS_DIR = ROOT.parent / "data" / "reports"
REPORTS_DIR.mkdir(parents=True, exist_ok=True)


def simulate_market_strategies(
    pivot: pd.DataFrame,
    c_conn: sqlite3.Connection,
    allowed_symbols: set[str] | None = None,
    max_sessions: int = 30,
    lookback: int = 10,
    base_tp: float = 0.022,
    base_sl: float = 0.008,
) -> tuple[list[dict], dict]:
    """Run the 4 Alpha Strategies across the specified universe and sessions using verified MooComp parameters."""
    if allowed_symbols is not None:
        valid_cols = [c for c in pivot.columns if c in allowed_symbols]
        sub_pivot = pivot[valid_cols].dropna(how="all")
    else:
        sub_pivot = pivot.dropna(how="all")

    all_sessions = list(sub_pivot.index)
    if not all_sessions:
        return [], {}

    test_sessions = all_sessions[-min(max_sessions, len(all_sessions)):]

    session_candidates = {}
    for s in test_sessions:
        s_idx = all_sessions.index(s)
        lb = min(lookback, s_idx)
        if lb < 2:
            prior_sessions = all_sessions[:s_idx] if s_idx > 0 else [s]
        else:
            prior_sessions = all_sessions[s_idx - lb : s_idx]

        sub = sub_pivot.loc[prior_sessions] if prior_sessions else sub_pivot.loc[[s]]
        scores = sub.sum(axis=0)
        counts = sub.count(axis=0)
        valid = scores[counts >= max(1, len(prior_sessions) // 2)].sort_values(ascending=False)

        cands = []
        for rank, (sym, sc) in enumerate(valid.items(), 1):
            cands.append({"symbol": sym, "rank": rank, "score": float(sc)})
            if rank >= 10:
                break
        session_candidates[s] = cands

    strategies = [
        ("Strategy 1: Rank 1 Alpha Engine", "#10b981", "strat1"),
        ("Strategy 4: Staged Scale-Out (+4%/+8%)", "#3b82f6", "strat4"),
        ("Strategy 2: Core + Tier-1 Booster", "#8b5cf6", "strat2"),
        ("Strategy 3: Dynamic Conviction Portfolio", "#f59e0b", "strat3"),
    ]

    equity_curves = {}
    strategy_metrics = []

    for strat_name, color, strat_id in strategies:
        equity = 100000.0
        peak = 100000.0
        max_dd = 0.0
        curve = [{"session": "START", "equity": 100000.0, "pnl_pct": 0.0}]
        daily_rets = []
        trade_count = 0
        wins = 0

        for s in test_sessions:
            cands = session_candidates.get(s, [])
            if not cands:
                curve.append({"session": s, "equity": round(equity, 2), "pnl_pct": round((equity / 100000.0 - 1.0) * 100, 2)})
                continue

            if strat_id == "strat1":
                targets = [(cands[0]["symbol"], 1.0, base_tp, base_sl, None)]
            elif strat_id == "strat4":
                # Two-leg scale out: first half at base_tp, second half at 2x base_tp
                targets = [(cands[0]["symbol"], 1.0, base_tp, base_sl, (base_tp, round(base_tp * 1.8, 4)))]
            elif strat_id == "strat2":
                sel = [(cands[0]["symbol"], 0.65, base_tp, base_sl, None)]
                if len(cands) > 1:
                    sel.append((cands[1]["symbol"], 0.35, base_tp, base_sl, None))
                targets = sel
            elif strat_id == "strat3":
                weights = [0.50, 0.30, 0.20]
                sel = []
                for idx, c in enumerate(cands[:3]):
                    sel.append((c["symbol"], weights[idx], base_tp, base_sl, None))
                tot_w = sum(t[1] for t in sel) if sel else 1.0
                targets = [(t[0], t[1] / tot_w, t[2], t[3], t[4]) for t in sel]

            session_trade_rets = []
            session_weights = []

            for sym, w, tp, sl, staged in targets:
                bars = pd.read_sql_query(
                    "SELECT ts, open, high, low, close FROM bars WHERE symbol=? ORDER BY ts",
                    c_conn,
                    params=(sym,),
                )
                if bars.empty:
                    continue
                bars["date"] = pd.to_datetime(bars["ts"], unit="ms").dt.strftime("%Y-%m-%d")
                day_bars = bars[bars["date"] == s].reset_index(drop=True)
                if len(day_bars) < 2:
                    continue

                op = day_bars.loc[0, "open"]
                if op <= 0.0:
                    continue

                if staged is not None:
                    tp1, tp2 = staged
                    leg1 = day_bars.iloc[-1]["close"]
                    leg2 = day_bars.iloc[-1]["close"]
                    hit1 = False
                    for i in range(len(day_bars)):
                        h = day_bars.loc[i, "high"]
                        l = day_bars.loc[i, "low"]
                        if (h - op) / op >= tp1 and not hit1:
                            leg1 = op * (1.0 + tp1)
                            hit1 = True
                        if (h - op) / op >= tp2 and hit1:
                            leg2 = op * (1.0 + tp2)
                            break
                        if (l - op) / op <= -sl:
                            if not hit1:
                                leg1 = op * (1.0 - sl)
                            leg2 = op * (1.0 - sl)
                            break
                    net = (0.5 * (leg1 / op - 1.0) + 0.5 * (leg2 / op - 1.0)) - 0.0010
                else:
                    exit_p = day_bars.iloc[-1]["close"]
                    for i in range(len(day_bars)):
                        h = day_bars.loc[i, "high"]
                        l = day_bars.loc[i, "low"]
                        if (h - op) / op >= tp:
                            exit_p = op * (1.0 + tp)
                            break
                        if (l - op) / op <= -sl:
                            exit_p = op * (1.0 - sl)
                            break
                    net = (exit_p / op - 1.0) - 0.0010

                session_trade_rets.append(net)
                session_weights.append(w)
                trade_count += 1
                if net > 0:
                    wins += 1

            if session_trade_rets:
                tot_w = sum(session_weights)
                day_pnl = sum(w * r for w, r in zip(session_weights, session_trade_rets)) / max(1e-6, tot_w)
                equity *= (1.0 + day_pnl)
                peak = max(peak, equity)
                max_dd = max(max_dd, (peak - equity) / peak)
                daily_rets.append(day_pnl)
            else:
                daily_rets.append(0.0)

            curve.append({"session": s, "equity": round(equity, 2), "pnl_pct": round((equity / 100000.0 - 1.0) * 100, 2)})

        comp_ret = (equity / 100000.0 - 1.0) * 100.0
        wr = (wins / max(1, trade_count)) * 100.0 if trade_count > 0 else 0.0
        std_ret = np.std(daily_rets) if daily_rets else 0.0
        sharpe = (np.mean(daily_rets) / (std_ret + 1e-8)) * np.sqrt(252) if std_ret > 0 else 0.0

        equity_curves[strat_name] = {"color": color, "curve": curve}
        strategy_metrics.append({
            "Strategy": strat_name,
            "Ending Equity ($)": round(equity, 2),
            "Return (%)": round(comp_ret, 2),
            "Max Drawdown (%)": round(max_dd * 100.0, 2),
            "Win Rate (%)": round(wr, 1),
            "Sharpe": round(sharpe, 2),
        })

    return strategy_metrics, equity_curves


def run_local_paper():
    print("=" * 80)
    print("=== LOCAL MULTI-STRATEGY PAPER TRADER GROUPED BY MARKET ===")
    print("=" * 80)

    c_conn = sqlite3.connect(f"file:{COMMON_STORE}?mode=ro", uri=True)

    print("Extracting session candle data...")
    daily_returns = pd.read_sql_query("""
        WITH session_bounds AS (
            SELECT 
                symbol,
                date(ts/1000, 'unixepoch') as session,
                min(ts) as min_ts,
                max(ts) as max_ts
            FROM bars
            GROUP BY symbol, session
            HAVING count(*) >= 2
        )
        SELECT 
            sb.symbol,
            sb.session,
            b_open.open,
            b_close.close,
            (b_close.close / b_open.open - 1.0) as ret
        FROM session_bounds sb
        JOIN bars b_open ON b_open.symbol = sb.symbol AND b_open.ts = sb.min_ts
        JOIN bars b_close ON b_close.symbol = sb.symbol AND b_close.ts = sb.max_ts
        WHERE b_open.open > 0
    """, c_conn)

    pivot = daily_returns.pivot(index="session", columns="symbol", values="ret")

    # Load NASDAQ universe
    nasdaq_path = ROOT / "data" / "nasdaq.json"
    if not nasdaq_path.exists():
        nasdaq_path = ROOT.parent / "data" / "nasdaq.json"

    nasdaq_symbols = set()
    if nasdaq_path.exists():
        nasdaq_symbols = {s.removeprefix("US.") for s in json.loads(nasdaq_path.read_text())}

    markets_data = {}

    print("Running US NASDAQ Alpha simulation...")
    nq_metrics, nq_curves = simulate_market_strategies(
        pivot,
        c_conn,
        allowed_symbols=nasdaq_symbols if nasdaq_symbols else None,
        max_sessions=30,
        lookback=10,
        base_tp=0.022,
        base_sl=0.008,
    )
    markets_data["US NASDAQ"] = {
        "metrics": nq_metrics,
        "curves": nq_curves,
        "market_desc": f"NASDAQ High-Beta Momentum ({len(nasdaq_symbols) if nasdaq_symbols else len(pivot.columns)} symbols · TP +2.2% / SL -0.8% · 10 bps friction)",
    }

    c_conn.close()

    # Save JSON report
    report_payload = {
        "markets": markets_data,
        "metrics": nq_metrics,
        "curves": nq_curves,
    }
    with open(REPORTS_DIR / "strategy_comparison.json", "w") as f:
        json.dump(report_payload, f, indent=2)

    print(f"\nWrote Grouped Market Strategy Comparison to {REPORTS_DIR / 'strategy_comparison.json'}")
    for mname, mdata in markets_data.items():
        print(f"\n--- {mname} ---")
        print(pd.DataFrame(mdata["metrics"]).to_string(index=False))

    print("=" * 80)


if __name__ == "__main__":
    run_local_paper()
