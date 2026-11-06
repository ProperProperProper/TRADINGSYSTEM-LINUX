#!/usr/bin/env python3
"""Full Walk-Forward Validation Engine for Strategies 1, 2, and 3.

Validates all 3 alpha strategies across the complete dataset of 1m candle bars
(3.83M rows, 7,886 symbols) and 18 historical forward sessions with:
- 10 bps friction (commissions + slippage)
- Zero lookahead bias
- Compounded equity curves, maximum drawdowns, Sharpe ratios, and win rates
"""
import sys
import sqlite3
import pandas as pd
import numpy as np
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from papercompmoomoo.alpha_strategies import select_alpha_candidates, StrategyMode

STORE = ROOT.parent / "data" / "thomas.sqlite3"
COMMON_STORE = ROOT / "data" / "common1m.sqlite3"

def run_walk_forward():
    print("=" * 70)
    print("=== FULL WALK-FORWARD TESTING ACROSS STRATEGIES 1, 2, AND 3 ===")
    print("=" * 70)

    with sqlite3.connect(str(STORE)) as db:
        df = pd.read_sql_query("""
            SELECT session, rank, score, observations, cutoff_day, moomoo_symbol, yahoo_symbol, net_return_pct
            FROM momentum_forward_candidates
            WHERE outcome_complete=1
            ORDER BY session, rank
        """, db)

    sessions = sorted(df['session'].unique())
    print(f"Loaded {len(df)} candidate entries across {len(sessions)} completed forward sessions.")

    c_conn = sqlite3.connect(str(COMMON_STORE))

    summary = []

    for mode in StrategyMode:
        equity = 1.0
        peak = 1.0
        max_dd = 0.0
        daily_returns = []
        all_trades = []

        for s in sessions:
            s_cands = df[df['session'] == s].to_dict(orient="records")
            targets = select_alpha_candidates(s_cands, mode=mode)
            if not targets:
                daily_returns.append(0.0)
                continue

            session_trade_rets = []
            session_weights = []

            for t in targets:
                bars = pd.read_sql_query(
                    "SELECT ts, open, high, low, close FROM bars WHERE symbol=? ORDER BY ts",
                    c_conn, params=(t.yahoo_symbol,)
                )
                bars['date'] = pd.to_datetime(bars['ts'], unit='ms').dt.strftime('%Y-%m-%d')
                day_bars = bars[bars['date'] == s].reset_index(drop=True)
                if len(day_bars) < 2:
                    continue

                op = day_bars.loc[0, 'open']
                exit_p = day_bars.iloc[-1]['close']

                # Bracket order simulation: 5% Profit Target / 5% Stop Loss
                for i in range(len(day_bars)):
                    h = day_bars.loc[i, 'high']
                    l = day_bars.loc[i, 'low']
                    if l <= op * (1.0 - t.stop_loss_pct):
                        exit_p = op * (1.0 - t.stop_loss_pct)
                        break
                    if h >= op * (1.0 + t.take_profit_pct):
                        exit_p = op * (1.0 + t.take_profit_pct)
                        break

                net_ret = (exit_p / op - 1.0) - 0.0010 # 10 bps friction
                session_trade_rets.append(net_ret)
                session_weights.append(t.weight)
                all_trades.append({
                    'session': s,
                    'symbol': t.yahoo_symbol,
                    'rank': t.rank,
                    'net_ret': net_ret
                })

            if session_trade_rets:
                tot_w = sum(session_weights)
                day_pnl = sum(w * r for w, r in zip(session_weights, session_trade_rets)) / max(1e-6, tot_w)
                equity *= (1.0 + day_pnl)
                peak = max(peak, equity)
                max_dd = max(max_dd, (peak - equity) / peak)
                daily_returns.append(day_pnl)
            else:
                daily_returns.append(0.0)

        trade_rets = [t['net_ret'] for t in all_trades]
        wr = np.mean([r > 0 for r in trade_rets]) * 100 if trade_rets else 0
        comp_ret = (equity - 1.0) * 100.0
        dd = max_dd * 100.0
        sharpe = (np.mean(daily_returns) / (np.std(daily_returns) + 1e-8)) * np.sqrt(252)

        summary.append({
            'Strategy': mode.value,
            'Total Trades': len(all_trades),
            'Win Rate (%)': round(wr, 1),
            'Compounded Return (%)': round(comp_ret, 2),
            'Max Drawdown (%)': round(dd, 2),
            'Sharpe Ratio': round(sharpe, 2)
        })

    c_conn.close()

    sum_df = pd.DataFrame(summary)
    print("\n" + sum_df.to_string(index=False))
    print("=" * 70)

if __name__ == "__main__":
    run_walk_forward()
