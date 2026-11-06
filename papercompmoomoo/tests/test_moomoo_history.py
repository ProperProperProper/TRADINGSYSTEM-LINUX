import sqlite3
import tempfile
import unittest
from pathlib import Path

import pandas as pd

from papercompmoomoo import moomoo_history
from papercompmoomoo.candlestore import CandleStore

NY = "America/New_York"


def bars(start, n, volume, price=10.0):
    index = pd.date_range(start, periods=n, freq="1min", tz=NY, name="Datetime")
    return pd.DataFrame({"Open": price, "High": price + 1, "Low": price - 1, "Close": price + 0.5, "Volume": volume}, index=index)


class History(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.store = CandleStore(self.dir / "c.sqlite3")
        self.addCleanup(self.store.close)
        self.snap = self.dir / "candles.sqlite3"
        con = sqlite3.connect(self.snap)
        con.execute("CREATE TABLE candles(symbol TEXT, kind TEXT, wall_ms INTEGER, open REAL, high REAL, low REAL, close REAL, volume REAL, turnover REAL)")
        self.con = con

    def fill(self, symbols, days=("2026-10-13", "2026-10-14")):
        for symbol in symbols:
            for day in days:
                f = bars(f"{day} 09:30", 20, 1200.0, price=20.0)
                for ts, row in zip(f.index, f.itertuples()):
                    self.con.execute("INSERT INTO candles VALUES(?, 'trades', ?, ?, ?, ?, ?, ?, 0)", (symbol, int(ts.timestamp() * 1000), row.Open, row.High, row.Low, row.Close, row.Volume))
        self.con.commit()

    def yahoo(self, symbol, volume=1000.0):
        for day in ("2026-10-13", "2026-10-14"):
            self.store.upsert_frame(symbol, bars(f"{day} 09:30", 20, volume, price=19.0), "yahoo", now_ms=1)

    def test_moomoo_history_is_converted_averaged_and_its_prices_stand(self):
        self.yahoo("AAA")
        self.yahoo("BRK-B")
        self.fill(["AAA", "BRK.B", "NEW"])
        r = moomoo_history.ingest(self.store, self.snap, now_ms=2)
        self.assertEqual((r["symbols"], r["converted"]), (3, 40), "only the second day of AAA and BRK.B has a ratio measured on an earlier day")
        first, second = (self.store.db.execute("SELECT open, volume, vol_y, vol_m, source FROM bars WHERE symbol='AAA' ORDER BY ts").fetchall()[i] for i in (0, 20))
        self.assertEqual(first, (20.0, 1100.0, 1000.0, 1200.0, "moomoo"), "day one: no earlier session, no conversion; the average of 1000 and 1200")
        self.assertEqual(second, (20.0, 1000.0, 1000.0, 1000.0, "moomoo"), "day two: ratio 1.2 measured on day one, 1200 / 1.2 = 1000; Moomoo's price stands")
        self.assertEqual(self.store.db.execute("SELECT count(*) FROM bars WHERE symbol='BRK-B'").fetchone()[0], 40, "BRK.B is stored under Yahoo's name")
        new = self.store.db.execute("SELECT volume, vol_y FROM bars WHERE symbol='NEW' ORDER BY ts LIMIT 1").fetchone()
        self.assertEqual(new, (1200.0, None), "no Yahoo bars, no ratio: Moomoo's own volume")

    def test_the_ratio_never_uses_the_future(self):
        self.yahoo("AAA")
        self.fill(["AAA"])
        frame = moomoo_history.read_snapshot(self.snap)["AAA"]
        before = moomoo_history.causal_ratios(self.store, "AAA", frame)
        # Yahoo's later days change a lot: every ratio of the earlier day must stay the same
        self.store.upsert_frame("AAA", bars("2026-10-14 09:30", 20, 9999.0, price=19.0), "yahoo", now_ms=3)
        after = moomoo_history.causal_ratios(self.store, "AAA", frame)
        self.assertEqual(list(before.iloc[:20]), list(after.iloc[:20]))
        self.assertEqual(list(before.iloc[:20]), [1.0] * 20)

    def test_a_live_moomoo_volume_is_kept_and_a_second_ingest_of_the_same_snapshot_is_skipped(self):
        self.yahoo("AAA")
        self.store.upsert_frame("AAA", bars("2026-10-13 09:30", 1, 500.0), "moomoo", now_ms=1)  # the live feed measured 500 for the first bar
        self.fill(["AAA"])
        moomoo_history.ingest(self.store, self.snap, now_ms=2)
        self.assertEqual(self.store.db.execute("SELECT vol_m FROM bars WHERE symbol='AAA' ORDER BY ts LIMIT 1").fetchone()[0], 500.0)
        self.assertIsNotNone(moomoo_history.ingest_if_changed(self.store, self.snap, now_ms=3))
        self.assertIsNone(moomoo_history.ingest_if_changed(self.store, self.snap, now_ms=4), "unchanged snapshot: nothing to do")

    def test_no_snapshot_is_not_an_error(self):
        self.assertEqual(moomoo_history.ingest(self.store, self.dir / "none.sqlite3")["bars"], 0)
        self.assertIsNone(moomoo_history.ingest_if_changed(self.store, self.dir / "none.sqlite3"))


if __name__ == "__main__":
    unittest.main()
