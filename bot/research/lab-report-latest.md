# Lab report (day 20743, mode: real)

Lab version 0.1.0

## 1. Data coverage
- 5220 runs, 11788 trades, 2 windows, 16 ingest batches
- quarantined files: 0
- degraded runs (missing feature columns): 0

## 1b. Paper record (forward data from the bot's paper account)
- source: ok | state: thin | 584 run segment(s), 42 closed trades, 3 days
- Latest paper account: equity 4983.83 USD from a start of 5000.00 USD (-0.32%), realised -16.17 USD, open P&L +0.00 USD.
- The record is thin (42 trades over 3 days; usable needs 30+ trades and 14+ days). Draw NO conclusions from it.
- Latest search chose signal  (hold 0 bars, top 0), robust in-sample score n/a, outcome 1 session.
- search-score vs forward-return check: insufficient (0 search/return pairs, 8 needed)

## 1c. Backtest archive by population (descriptive only; selection bias applies to top-ranked and chosen runs)
- chosen [successful_only]: 678 runs (678 with trades), 7848 trades, mean return on margin per trade +0.0104, win rate 62%, mean run return +2.2%
- oos [header_only]: 90 runs (0 with trades), 0 trades, mean return on margin per trade n/a, win rate n/a, mean run return -0.0%
- oos [none]: 12 runs (12 with trades), 28 trades, mean return on margin per trade -0.0074, win rate 36%, mean run return -0.5%
- random [header_only]: 3391 runs (0 with trades), 0 trades, mean return on margin per trade n/a, win rate n/a, mean run return -0.0%
- random [none]: 1049 runs (1049 with trades), 3912 trades, mean return on margin per trade -0.0072, win rate 30%, mean run return -1.3%

## 2. Operations
- data.sqlite3: 23.6 MB of 10737 MB limit; memory.sqlite3: 5.39 MB

## 3. Alerts
- none
