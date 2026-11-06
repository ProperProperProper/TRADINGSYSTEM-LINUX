"""Event study of the papercompmoomoo entry rule on real 1-minute candles (TradingSystem's merged store, read-only).

Entry rule as the bot codes it (bot.py, scan step): RVOL = sum of the volume of the whole 5-day snapshot / average daily volume of the 21 prior sessions,
trigger when RVOL >= 5 and the last price is >= 3% above the previous close. Exits: stop 2%, trailing stop 3%, 90 minutes at most. We enter at the NEXT bar's
open (the signal uses the bar that just closed, so there is no look-ahead), one entry per symbol per day, long only.

Three RVOL definitions are compared: "as coded" (5-day volume / one day's average, which is about 5 for an ordinary stock), "today" (today's cumulative volume
up to the signal bar / average daily volume) and "time-matched" (today's cumulative volume / the average cumulative volume of up to four earlier sessions at the same
time of day, which is what the bot now uses). A baseline applies the same exits to an entry at 10:30 ET of every
symbol-day, so the event returns are judged against what the exit rule does by itself.
"""
import math
import sqlite3
import statistics
import sys
from collections import defaultdict
from datetime import datetime
from zoneinfo import ZoneInfo

NY = ZoneInfo("America/New_York")
STORE = sys.argv[1] if len(sys.argv) > 1 else "../data/thomas.sqlite3"
STOP, TRAIL, HOLD_BARS, RVOL, MOVE = 0.02, 0.03, 6, 5.0, 0.03
COST = float(sys.argv[2]) if len(sys.argv) > 2 else 0.0010  # round-trip, as a fraction (10 bps)


def load():
    con = sqlite3.connect(f"file:{STORE}?mode=ro", uri=True)
    bars = defaultdict(lambda: defaultdict(list))
    for sym, ms, o, h, l, c, v in con.execute("SELECT moomoo_symbol, wall_ms, open, high, low, close, volume FROM candles_merged ORDER BY moomoo_symbol, wall_ms"):
        day = datetime.fromtimestamp(ms / 1000, NY).date()
        bars[sym][day].append((ms, o, h, l, c, v))
    return bars


def trade(day_bars, entry_i):
    """Simulate the bot's exits from the open of bar entry_i. Returns the return of the position (fraction)."""
    entry = day_bars[entry_i][1]
    peak = entry
    for k in range(entry_i, min(entry_i + HOLD_BARS, len(day_bars))):
        _, o, h, l, c, _ = day_bars[k]
        stop_px = entry * (1 - STOP)
        peak = max(peak, h)
        trail_px = peak * (1 - TRAIL)
        level = max(stop_px, trail_px)
        if o <= level:
            return o / entry - 1  # gapped through the stop
        if l <= level:
            return level / entry - 1
        if k == entry_i + HOLD_BARS - 1 or k == len(day_bars) - 1:
            return c / entry - 1
    return 0.0


def main():
    bars = load()
    events = {"as coded": [], "today": [], "time-matched": []}
    baseline = []
    for sym, days in bars.items():
        order = sorted(days)
        for di, day in enumerate(order):
            prior = order[max(0, di - 21):di]
            if len(prior) < 10 or di == 0:
                continue
            avg_daily = statistics.fmean(sum(b[5] for b in days[p]) for p in prior)
            prev_close = days[order[di - 1]][-1][4]
            db = days[day]
            if len(db) < 12 or avg_daily <= 0 or prev_close <= 0:
                continue
            if len(db) > 4:
                baseline.append(trade(db, 4))
            last5 = [days[p] for p in order[max(0, di - 4):di]]
            fired = set()
            cum = 0.0
            # cumulative volume of up to four earlier sessions at each bar index (the bot's 5-day frame holds today plus four)
            ref = [days[p] for p in order[max(0, di - 4):di] if len(days[p]) == len(db)]
            ref_cum = [[sum(b[5] for b in d[: k + 1]) for k in range(len(d))] for d in ref]
            for i in range(len(db) - 1):
                cum += db[i][5]
                move = db[i][4] / prev_close - 1
                if move < MOVE:
                    continue
                five_day = sum(b[5] for d in last5 for b in d) + cum
                matched = statistics.fmean(rc[i] for rc in ref_cum) if len(ref_cum) >= 2 else 0.0
                rows = [("as coded", five_day / avg_daily), ("today", cum / avg_daily)]
                if matched > 0:
                    rows.append(("time-matched", cum / matched))
                for name, rv in rows:
                    if rv >= RVOL and name not in fired:
                        fired.add(name)
                        events[name].append((sym, day, trade(db, i + 1)))
    base_mean = statistics.fmean(baseline)
    base_sd = statistics.pstdev(baseline)
    print(f"symbols {len(bars)}; sessions per symbol {statistics.median(len(d) for d in bars.values())}; cost assumption {COST*1e4:.0f} bps round trip")
    print(f"baseline (same exits, entry 10:30 ET of every symbol-day): n={len(baseline)} mean {100*base_mean:+.3f}% sd {100*base_sd:.2f}%")
    for name, ev in events.items():
        r = [x[2] for x in ev]
        if len(r) < 2:
            print(f"{name:9}: {len(r)} events")
            continue
        m, sd = statistics.fmean(r), statistics.pstdev(r)
        se = math.sqrt(sd**2 / len(r) + base_sd**2 / len(baseline))
        wins = sum(1 for x in r if x > 0) / len(r)
        days = len({x[1] for x in ev})
        print(f"{name:9}: n={len(r)} on {days} days, {len({x[0] for x in ev})} symbols | mean {100*m:+.3f}% gross, {100*(m-COST):+.3f}% net | win rate {100*wins:.0f}% | vs baseline {100*(m-base_mean):+.3f}% (t={ (m-base_mean)/se:+.2f})")


main()
