import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pandas as pd

from papercompmoomoo.candlestore import CandleStore, backfill_short, pending_symbols, request_start, update_symbols

NOW = datetime(2026, 10, 19, 15, 1, tzinfo=timezone.utc)  # 11:01 New York: the 10:45 bar (14:45 UTC) is the newest closed one


def bars(start_utc: str, n: int, price: float = 100.0, volume: float = 1000.0):
    index = pd.date_range(start_utc, periods=n, freq="1min", tz="UTC").tz_convert("America/New_York")
    index.name = "Datetime"
    return pd.DataFrame({"Open": price, "High": price + 1, "Low": price - 1, "Close": price + 0.5, "Volume": volume}, index=index)


def multi(frames: dict):
    return pd.concat(frames, axis=1)


class Store(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.store = CandleStore(self.dir / "c.sqlite3")
        self.addCleanup(self.store.close)

    def test_bars_round_trip_with_new_york_time_and_replace_on_the_same_time(self):
        self.assertEqual(self.store.upsert_frame("AAA", bars("2026-10-19 13:30", 4)), 4)
        self.store.upsert_frame("AAA", bars("2026-10-19 14:00", 2, price=200.0))  # overlaps the last two bars
        frame = self.store.frame("AAA")
        self.assertEqual(len(frame), 4)
        self.assertEqual(str(frame.index.tz), "America/New_York")
        self.assertEqual(frame.index[0].hour, 9)
        self.assertEqual(list(frame["Close"]), [100.5, 100.5, 200.5, 200.5])
        self.assertEqual(self.store.last_ts(["AAA", "ZZZ"]), {"AAA": int(pd.Timestamp("2026-10-19 14:15", tz="UTC").timestamp() * 1000)})

    def test_only_what_is_new_is_requested_and_an_up_to_date_symbol_is_not_requested_at_all(self):
        self.store.upsert_frame("OLD", bars("2026-10-16 18:00", 8))        # Friday: behind
        self.store.upsert_frame("FRESH", bars("2026-10-19 13:30", 7))      # up to 15:00 UTC: it already holds the bar after the newest closed one (14:45)
        calls = []

        def fetch(chunk, start):
            calls.append((list(chunk), start))
            return multi({t: bars("2026-10-19 13:30", 6) for t in chunk}) if len(chunk) > 1 else multi({chunk[0]: bars("2026-10-19 13:30", 6)})

        stats = update_symbols(self.store, ["OLD", "FRESH", "NEW"], NOW, fetch, backfill_days=35)
        self.assertEqual(stats["skipped_up_to_date"], 1)
        self.assertEqual(len(calls), 1)
        self.assertNotIn("FRESH", calls[0][0])
        self.assertEqual(calls[0][0][0], "NEW", "a never-seen symbol goes first")
        # the chunk holds a never-seen symbol, so it starts at the backfill date, not at the old symbol's last bar
        self.assertEqual(calls[0][1], NOW - timedelta(days=35))

    def test_a_known_symbol_starts_at_its_last_bar_with_one_bar_of_overlap(self):
        last = int(pd.Timestamp("2026-10-19 13:45", tz="UTC").timestamp() * 1000)
        self.assertEqual(request_start(last, NOW, 35), datetime(2026, 10, 19, 13, 0, tzinfo=timezone.utc), "three bars of overlap: late revisions land")
        self.assertEqual(request_start(None, NOW, 35), NOW - timedelta(days=35))
        ancient = int(pd.Timestamp("2026-01-02", tz="UTC").timestamp() * 1000)
        self.assertEqual(request_start(ancient, NOW, 35), NOW - timedelta(days=58), "never before Yahoo's limit")

    def test_a_last_bar_equal_to_the_newest_closed_one_is_asked_for_again_because_it_may_have_been_stored_in_progress(self):
        self.store.upsert_frame("AAA", bars("2026-10-19 13:30", 6))  # last bar 14:45 UTC = the newest closed bar at NOW
        pending, _ = pending_symbols(self.store, ["AAA"], NOW)
        self.assertEqual(pending, ["AAA"])

    def test_given_order_is_kept_for_priority_symbols(self):
        self.store.upsert_frame("A", bars("2026-10-16 18:00", 2))
        pending, _ = pending_symbols(self.store, ["Z", "A", "M"], NOW, keep_order=True)
        self.assertEqual(pending, ["Z", "A", "M"])

    def test_second_run_in_the_same_minute_makes_no_request(self):
        def fetch(chunk, start):
            return multi({t: bars("2026-10-19 13:30", 7) for t in chunk}) if len(chunk) > 1 else multi({chunk[0]: bars("2026-10-19 13:30", 7)})

        update_symbols(self.store, ["AAA", "BBB"], NOW, fetch)
        calls = []
        stats = update_symbols(self.store, ["AAA", "BBB"], NOW, lambda c, s: calls.append(c))
        self.assertEqual((calls, stats["requests"], stats["skipped_up_to_date"]), ([], 0, 2))

    def test_a_failed_download_is_counted_and_leaves_the_store_as_it_was(self):
        stats = update_symbols(self.store, ["AAA"], NOW, lambda c, s: None)
        self.assertEqual((stats["failed"], stats["bars"]), (1, 0))
        self.assertEqual(self.store.last_ts(["AAA"]), {})

    def test_shortlist_comes_from_the_disk_baselines(self):
        for symbol, volume in [("AAA", 5.0), ("BBB", 9.0), ("CCC", 1.0), ("OLD", 99.0)]:
            self.store.set_baseline(symbol, volume, "2026-10-19" if symbol != "OLD" else "2026-09-01")
        self.assertEqual(self.store.shortlist(["AAA", "BBB", "CCC", "OLD"], 2, "2026-10-12"), ["BBB", "AAA"])
        self.store.set_baseline("THIN", 0.0, "2026-10-19")  # checked, too little history
        self.assertEqual(self.store.baseline_count("2026-10-12"), 3, "a symbol with no usable baseline is not counted")
        self.assertIn("THIN", self.store.fresh_symbols("2026-10-12"), "but it is known, so it is not asked for again")
        self.assertNotIn("THIN", self.store.shortlist(["AAA", "THIN"], 5, "2026-10-12"))

    def test_junk_is_refused_on_the_way_in_and_cleaned_if_it_is_already_there(self):
        index = pd.DatetimeIndex(["2026-10-19 09:30", "2026-10-19 09:45", "2026-10-19 10:00", "2026-10-19 10:15", "2026-10-19 10:30"], tz="America/New_York")
        good = pd.DataFrame({"Open": 10.0, "High": 10.1, "Low": 9.9, "Close": 10.0, "Volume": [100.0, 0.0, 120.0, 130.0, 140.0]}, index=index)
        off_grid = pd.DataFrame({"Open": 10.0, "High": 10.0, "Low": 10.0, "Close": 10.0, "Volume": 0.0}, index=pd.DatetimeIndex(["2026-10-19 09:43:49"], tz="America/New_York"))
        impossible = pd.DataFrame({"Open": 10.0, "High": 9.0, "Low": 9.5, "Close": 10.0, "Volume": 50.0}, index=pd.DatetimeIndex(["2026-10-19 11:00"], tz="America/New_York"))
        self.assertEqual(self.store.upsert_frame("AAA", pd.concat([good, off_grid, impossible])), 4, "the zero-volume bar, the off-grid row and the impossible bar never get in")
        # junk that is already stored (older versions): clean removes it, a repeated print keeps its first copy and the newest bar is left alone
        with self.store.db:
            self.store.db.executemany("INSERT INTO bars(symbol,ts,open,high,low,close,volume) VALUES(?,?,?,?,?,?,?)", [("BBB", 60_000 * 5, 10, 10.1, 9.9, 10, 0), ("BBB", 60_000 * 5 + 7, 10, 10.1, 9.9, 10, 5),
                                                                                    ("BBB", 60_000 * 6, 10, 10.1, 9.9, 10, 77), ("BBB", 60_000 * 7, 10, 10.1, 9.9, 10, 77),
                                                                                    ("BBB", 60_000 * 8, 10, 10.1, 9.9, 10, 77)])
        counts = self.store.clean()
        self.assertEqual((counts["off_grid"], counts["no_volume"], counts["repeated"]), (1, 1, 1))
        left = [r[0] for r in self.store.db.execute("SELECT ts FROM bars WHERE symbol='BBB' ORDER BY ts")]
        self.assertEqual(left, [60_000 * 6, 60_000 * 8], "the repeated middle copy is gone, the first and the newest stay")
        self.assertIsNone(self.store.clean_if_due(now=datetime.now(timezone.utc)) and None)

    def test_clean_runs_at_most_once_per_day(self):
        now = datetime(2026, 10, 19, 12, 0, tzinfo=timezone.utc)
        self.assertIsNotNone(self.store.clean_if_due(now=now))
        self.assertIsNone(self.store.clean_if_due(now=now + timedelta(hours=3)))
        self.assertIsNotNone(self.store.clean_if_due(now=now + timedelta(hours=21)))

    def test_a_measured_scale_is_remembered_and_unmeasured_symbols_get_the_typical_one(self):
        self.assertIsNone(self.store.scale("AAA"))
        self.assertEqual(self.store.typical_scale(0.8), 0.8, "fewer than ten measurements: the default")
        for i in range(10):
            self.store.set_scale(f"S{i}", 0.5 + i * 0.05, "t")
        self.assertEqual(self.store.scale("S3"), 0.65)
        self.assertAlmostEqual(self.store.typical_scale(0.8), 0.75)

    def test_pending_order_is_never_seen_first_then_oldest(self):
        self.store.upsert_frame("A", bars("2026-10-19 13:30", 3))
        self.store.upsert_frame("B", bars("2026-10-16 18:00", 2))
        pending, _ = pending_symbols(self.store, ["A", "B", "C"], NOW)
        self.assertEqual(pending, ["C", "B", "A"])

    def test_a_symbol_with_a_short_disk_history_gets_its_full_history_once_and_a_zero_baseline_is_only_trusted_after_that(self):
        self.store.upsert_frame("SHORT", bars("2026-10-19 13:30", 8))  # one day on disk; Yahoo has weeks
        self.store.upsert_frame("FULL", bars("2026-09-21 13:30", 8))
        for day in range(22, 31):
            self.store.upsert_frame("FULL", bars(f"2026-09-{day} 13:30", 8))
        calls = []

        def fetch(chunk, start):
            calls.append((list(chunk), start))
            return multi({t: bars("2026-09-21 13:30", 400) for t in chunk}) if len(chunk) > 1 else bars("2026-09-21 13:30", 400)

        stats = backfill_short(self.store, ["SHORT", "FULL"], NOW, fetch, sessions=5, days=35)
        self.assertEqual((stats["short"], len(calls), calls[0][0]), (1, 1, ["SHORT"]))
        self.assertLess((NOW - calls[0][1]).days, 36)
        self.assertGreater(self.store.session_counts(["SHORT"])["SHORT"], 5)
        self.assertEqual(backfill_short(self.store, ["SHORT"], NOW, fetch, sessions=500)["short"], 0, "asked once, not every start")
        self.store.set_baseline("SHORT", 0.0, "2026-10-20")
        self.store.set_baseline("NEVER", 0.0, "2026-10-20")
        self.assertEqual(self.store.forget_unverified_zero_baselines(), 1, "only the backfilled symbol keeps its zero")
        self.assertEqual(self.store.fresh_symbols("2026-10-01"), {"SHORT"})


    def test_a_row_is_stamped_when_it_changes_and_not_when_it_is_rewritten_unchanged(self):
        self.store.upsert_frame("AAA", bars("2026-10-19 13:30", 2), now_ms=1_000)
        self.store.upsert_frame("AAA", bars("2026-10-19 13:30", 2), now_ms=2_000)  # identical: no change, no new stamp
        self.assertEqual([r[0] for r in self.store.db.execute("SELECT updated_ms FROM bars")], [1_000, 1_000])
        self.store.upsert_frame("AAA", bars("2026-10-19 13:45", 1, volume=5000.0), now_ms=3_000)  # revised volume on the second bar
        self.assertEqual([r[0] for r in self.store.db.execute("SELECT updated_ms FROM bars ORDER BY ts")], [1_000, 3_000])


    def test_the_volume_is_the_average_of_the_two_feeds_as_soon_as_both_are_there_in_either_order(self):
        yahoo, moomoo = bars("2026-10-19 13:30", 1, volume=1000.0), bars("2026-10-19 13:30", 1, volume=800.0)
        self.store.upsert_frame("AAA", yahoo, "yahoo", now_ms=1_000)
        self.assertEqual(self.store.db.execute("SELECT volume FROM bars").fetchone()[0], 1000.0, "one feed: its own volume")
        self.store.upsert_frame("AAA", moomoo, "moomoo", now_ms=2_000)
        self.assertEqual(self.store.db.execute("SELECT volume, vol_y, vol_m FROM bars").fetchone(), (900.0, 1000.0, 800.0))
        self.store.upsert_frame("BBB", moomoo, "moomoo", now_ms=1_000)
        self.store.upsert_frame("BBB", yahoo, "yahoo", now_ms=2_000)
        self.assertEqual(self.store.db.execute("SELECT volume FROM bars WHERE symbol='BBB'").fetchone()[0], 900.0, "the other order gives the same average")
        self.store.upsert_frame("BBB", bars("2026-10-19 13:30", 1, volume=1200.0), "yahoo", now_ms=3_000)  # Yahoo revises its volume: the average follows
        self.assertEqual(self.store.db.execute("SELECT volume FROM bars WHERE symbol='BBB'").fetchone()[0], 1000.0)


    def test_old_yahoo_only_bars_are_pruned_but_bars_with_a_moomoo_volume_stay(self):
        now = datetime(2026, 10, 19, 15, 0, tzinfo=timezone.utc)
        self.store.upsert_frame("AAA", bars("2026-07-01 13:30", 4), "yahoo", now_ms=1)
        self.store.upsert_frame("BBB", bars("2026-07-01 13:30", 4), "yahoo", now_ms=1)
        self.store.upsert_frame("BBB", bars("2026-07-01 13:30", 2), "moomoo", now_ms=2)
        self.store.upsert_frame("AAA", bars("2026-10-19 13:30", 2), "yahoo", now_ms=3)
        self.store.prune(60, now)
        counts = dict(self.store.db.execute("SELECT symbol, count(*) FROM bars GROUP BY symbol"))
        self.assertEqual(counts, {"AAA": 2, "BBB": 2}, "old Yahoo-only bars are pruned; only BBB's two bars that hold a Moomoo volume stay")


    def test_a_flat_candle_with_no_trade_is_kept_and_a_zero_volume_bar_that_moved_is_not(self):
        flat = bars("2026-10-19 13:30", 3, volume=0.0)  # build bars, then make them flat: open = high = low = close
        flat["High"], flat["Low"], flat["Open"], flat["Close"] = 50.0, 50.0, 50.0, 50.0
        self.assertEqual(self.store.upsert_frame("THIN", flat), 3, "no trade is no trade: the candles stay")
        moved = bars("2026-10-19 14:15", 2, volume=0.0)  # zero volume but a range: not a candle of anything
        self.assertEqual(self.store.upsert_frame("THIN", moved), 0)
        self.assertEqual(self.store.clean()["no_volume"], 0)
        self.assertEqual(self.store.clean()["repeated"], 0, "identical no-trade candles are not a repeated print")
        self.assertEqual(self.store.db.execute("SELECT count(*), min(volume) FROM bars").fetchone(), (3, 0.0))
        # Moomoo's real volume for the same candle beats Yahoo's zero instead of being halved
        self.store.upsert_frame("THIN", bars("2026-10-19 13:30", 1, price=50.0, volume=800.0).assign(High=50.0, Low=50.0, Open=50.0, Close=50.0), "moomoo", now_ms=2)
        self.assertEqual(self.store.db.execute("SELECT volume FROM bars ORDER BY ts LIMIT 1").fetchone()[0], 800.0)


if __name__ == "__main__":
    unittest.main()
