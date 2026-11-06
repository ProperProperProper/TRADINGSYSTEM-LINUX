# Causal Intraday Synthetic GEX & Walk-Forward Engine

A point-in-time, causal microstructural framework designed to approximate dealer gamma exposure (GEX), Call Walls, Put Walls, and Gamma Flip levels using **only 1-minute OHLCV candle and volume data**.

---

## 1. Mathematical Framework

The model reconstructs institutional options positioning and market maker hedging dynamics without lookahead bias.

### A. Causal Tick Friction ($F_i$)
At any given 1-minute bar $i$, price displacement resistance is modeled as volume traded per unit of price range:

$$F_i = \frac{V_i}{\max(H_i - L_i, \; \epsilon)}$$

* $V_i$: Bar volume.
* $H_i - L_i$: High-to-low price spread.
* $\epsilon$: Tick-size protection constant ($0.01$) to prevent division by zero on flat bars.

### B. Discrete Strike Quantization ($K_i$)
Each 1-minute bar is mapped to standard options strike intervals:

$$K_i = \operatorname{round}\left(\frac{\text{Typical Price}_i}{\text{Strike Step}}\right) \times \text{Strike Step}$$

$$\text{Typical Price}_i = \frac{H_i + L_i + C_i}{3}$$

* **Strike Step:** Standardized at `$5.00` for high-beta US equities ($100–$400 range like APP), or `$1.00`/`$2.50` for lower-priced equities.

### C. Exponential Time Decay Memory
To simulate shifting dealer inventory and expiring intraday impact, historical friction within the session decays exponentially:

$$w_i = e^{-\lambda (t - i)}, \quad \lambda = \frac{\ln(2)}{\tau}$$

* $t$: Current 1-minute bar index ($0 \le i < t$).
* $\tau$: Half-life decay constant in bars (e.g., 45 minutes).

### D. Dynamic Decayed Strike Density ($FD_K(t)$)
The total point-in-time absorption density at strike $K$ is:

$$FD_K(t) = \sum_{i=0}^{t-1} \left( F_i \cdot w_i \cdot \mathbb{I}_{\{K_i = K\}} \right)$$

* **Synthetic Call Wall ($CW_t$):** $\operatorname{argmax}_{K > S_t} FD_K(t)$
* **Synthetic Put Wall ($PW_t$):** $\operatorname{argmax}_{K < S_t} FD_K(t)$
* **Synthetic Gamma Flip ($GF_t$):** Running cumulative session VWAP up to bar $t-1$:

$$GF_t = \frac{\sum_{i=0}^{t-1} V_i \cdot \text{Typical Price}_i}{\sum_{i=0}^{t-1} V_i}$$

---

## 2. Long-Only Execution State Machine

```
At Bar t Close (Price = S_t):
  │
  ├── 1. Regime Assessment:
  │      ├── IF S_t < GF_t:
  │      │     REGIME = NEGATIVE_GAMMA (Distributive / Cascading Risk)
  │      │     └── ACTION: FORBID LONGS. If long, trigger immediate defensive exit.
  │      │
  │      └── IF S_t >= GF_t:
  │            REGIME = POSITIVE_GAMMA (Constructive / Stabilized)
  │
  ├── 2. Position Management (If Holding Long):
  │      ├── Exit 1 (Regime Flip): Close below GF_t
  │      └── Exit 2 (Failed Breakout): Close below CW_t * (1 - StopBuffer)
  │
  └── 3. Entry Triggers (If Flat & Positive Gamma):
         └── Breakout Squeeze: S_t > CW_t * (1 + BreakoutBuffer)
```

---

## 3. Production Python Engine & Walk-Forward Harness

```python
import numpy as np
import pandas as pd
from itertools import product
from typing import Dict, List, Tuple

class CausalSyntheticGEX:
    """
    Point-in-time intraday friction engine.
    Zero lookahead: bar t only evaluates historical bars 0 through t-1.
    """
    def __init__(self, strike_step: float = 5.0, half_life_bars: int = 45, warmup_bars: int = 20):
        self.strike_step = strike_step
        self.decay_lambda = np.log(2.0) / max(half_life_bars, 1)
        self.warmup_bars = warmup_bars
        self.history = []
        self.cum_vol_price = 0.0
        self.cum_vol = 0.0

    def update(self, bar_idx: int, open_: float, high: float, low: float, close: float, volume: float) -> dict:
        typical_price = (high + low + close) / 3.0
        price_range = max(high - low, 0.01)
        friction = volume / price_range
        strike_bin = round(typical_price / self.strike_step) * self.strike_step

        # Update point-in-time running VWAP (Gamma Flip proxy)
        self.cum_vol_price += volume * typical_price
        self.cum_vol += volume
        gamma_flip = self.cum_vol_price / max(self.cum_vol, 1.0)

        self.history.append({
            'idx': bar_idx,
            'strike': strike_bin,
            'friction': friction
        })

        if bar_idx < self.warmup_bars:
            return {
                "spot": close,
                "regime": "WARMUP",
                "call_wall": None,
                "put_wall": None,
                "gamma_flip": gamma_flip
            }

        # Apply exponential decay to past bars
        elapsed = bar_idx - np.array([h['idx'] for h in self.history])
        decay_weights = np.exp(-self.decay_lambda * elapsed)
        frictions = np.array([h['friction'] for h in self.history])
        strikes = np.array([h['strike'] for h in self.history])

        unique_strikes = np.unique(strikes)
        strike_friction = {}
        for s in unique_strikes:
            mask = (strikes == s)
            strike_friction[s] = np.sum(frictions[mask] * decay_weights[mask])

        strikes_above = [k for k in unique_strikes if k > close]
        strikes_below = [k for k in unique_strikes if k < close]

        call_wall = max(strikes_above, key=lambda k: strike_friction[k]) if strikes_above else (close + self.strike_step)
        put_wall = max(strikes_below, key=lambda k: strike_friction[k]) if strikes_below else (close - self.strike_step)

        regime = "POSITIVE_GAMMA" if close >= gamma_flip else "NEGATIVE_GAMMA"

        return {
            "spot": close,
            "regime": regime,
            "call_wall": float(call_wall),
            "put_wall": float(put_wall),
            "gamma_flip": float(gamma_flip)
        }


def simulate_single_session(day_df: pd.DataFrame, params: dict) -> List[dict]:
    """Simulates trading on a single session's 1-minute bars."""
    engine = CausalSyntheticGEX(
        strike_step=params['strike_step'],
        half_life_bars=params['half_life_bars'],
        warmup_bars=params['warmup_bars']
    )
    trades = []
    position = 0
    entry_price = 0.0
    entry_bar = 0

    for t, row in day_df.reset_index(drop=True).iterrows():
        state = engine.update(
            bar_idx=t,
            open_=row['open'],
            high=row['high'],
            low=row['low'],
            close=row['close'],
            volume=row['volume']
        )
        if state['regime'] == "WARMUP":
            continue

        spot = state['spot']
        flip = state['gamma_flip']
        cw = state['call_wall']

        # Check Position Exits
        if position == 1:
            hit_flip_stop = spot < flip
            hit_wall_stop = spot < cw * (1 - params['stop_loss_pct'])

            if hit_flip_stop or hit_wall_stop:
                pnl = (spot - entry_price) / entry_price
                trades.append({
                    "entry_price": entry_price,
                    "exit_price": spot,
                    "return_pct": pnl,
                    "bars_held": t - entry_bar,
                    "exit_reason": "FLIP_BREACH" if hit_flip_stop else "WALL_REJECT"
                })
                position = 0

        # Check Long Entry
        elif position == 0 and state['regime'] == "POSITIVE_GAMMA":
            if spot > cw * (1 + params['breakout_buffer']):
                position = 1
                entry_price = spot
                entry_bar = t

    # Close any open trade at market on final session bar
    if position == 1:
        final_spot = day_df['close'].iloc[-1]
        trades.append({
            "entry_price": entry_price,
            "exit_price": final_spot,
            "return_pct": (final_spot - entry_price) / entry_price,
            "bars_held": len(day_df) - entry_bar,
            "exit_reason": "EOD_CLOSE"
        })

    return trades


def evaluate_sharpe(trade_list: List[dict]) -> float:
    """Calculates annualized trade Sharpe ratio."""
    if not trade_list:
        return -999.0
    returns = np.array([t['return_pct'] for t in trade_list])
    if len(returns) < 5 or np.std(returns) == 0:
        return -999.0
    return float(np.mean(returns) / np.std(returns) * np.sqrt(252 * 5))


def run_walk_forward(
    session_dict: Dict[str, pd.DataFrame],
    param_grid: dict,
    train_days: int = 20,
    test_days: int = 5
) -> pd.DataFrame:
    """
    Rolling In-Sample (Train) Optimization -> Out-of-Sample (Test) Walk-Forward Harness.
    """
    dates = sorted(session_dict.keys())
    total_days = len(dates)
    all_oos_trades = []

    keys, values = zip(*param_grid.items())
    param_combinations = [dict(zip(keys, v)) for v in product(*values)]

    for start_idx in range(0, total_days - train_days - test_days + 1, test_days):
        train_dates = dates[start_idx : start_idx + train_days]
        test_dates = dates[start_idx + train_days : start_idx + train_days + test_days]

        print(f"\n[WF Window] Train: {train_dates[0]} to {train_dates[-1]} | Test: {test_dates[0]} to {test_dates[-1]}")

        # In-Sample Parameter Calibration
        best_score = -999.0
        best_params = param_combinations[0]

        for p in param_combinations:
            is_trades = []
            for d in train_dates:
                is_trades.extend(simulate_single_session(session_dict[d], p))
            score = evaluate_sharpe(is_trades)
            if score > best_score:
                best_score = score
                best_params = p

        print(f"  Best IS Score: {best_score:.2f} | Locked Params: {best_params}")

        # Out-of-Sample Execution (Unseen Test Block)
        for d in test_dates:
            oos_trades = simulate_single_session(session_dict[d], best_params)
            for trade in oos_trades:
                trade['date'] = d
                trade['params'] = str(best_params)
                all_oos_trades.append(trade)

    return pd.DataFrame(all_oos_trades)
```

---

## 4. Input Data Format & Running Walk-Forward Tests

Your saved 1-minute candle data should be passed as a dictionary where keys are session dates and values are 1-minute DataFrames:

```python
if __name__ == "__main__":
    # Parameter grid for calibration
    GRID = {
        'strike_step': [5.0],
        'half_life_bars': [30, 45, 60],
        'warmup_bars': [20],
        'breakout_buffer': [0.001, 0.002],
        'stop_loss_pct': [0.005, 0.008]
    }

    # Format: session_dict = {'2026-09-01': df_day1, '2026-09-02': df_day2, ...}
    # Each DataFrame requires columns: ['open', 'high', 'low', 'close', 'volume']
    
    # Example execution:
    # oos_results = run_walk_forward(session_dict, GRID, train_days=20, test_days=5)
    # print(oos_results.describe())
```

---

## 5. Walk-Forward Operational Rules

1. **Daily Reset Invariant:** The `CausalSyntheticGEX` instance must be freshly instantiated at bar 0 (`09:30 AM EST`) of every session. Never leak friction, volume, or VWAP accumulators across overnight session boundaries.
2. **Warmup Window:** The initial 20 bars (`09:30–09:50 AM EST`) are strictly for state initialization. No trade entries are allowed during this window to avoid low-sample volume distortion.
3. **Walk-Forward Efficiency (WFE):** Compute the robustness metric across all windows:
   $$\text{WFE} = \frac{\text{Sharpe}_{\text{OOS}}}{\text{Sharpe}_{\text{IS}}}$$
   Reject parameter configurations where $\text{WFE} < 0.50$.