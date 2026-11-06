import json
import sqlite3
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

import pandas as pd

from papercompmoomoo import converter
from papercompmoomoo.candlestore import CandleStore

NY = "America/New_York"


def frame(rows):
    idx = pd.DatetimeIndex([pd.Timestamp(t, tz=NY) for t, *_ in rows], name="Datetime")
    return pd.DataFrame({"Open": [r[1] for r in rows], "High": [r[2] for r in rows], "Low": [r[3] for r in rows], "Close": [r[4] for r in rows], "Volume": [r[5] for r in rows]}, index=idx)


class FakeFeed:
    def __init__(self, bars):
        self.bars, self.released = bars, None

    def ensure(self, codes, push=False):
        self.pushed = push
        return set(codes)

    def common_candles(self, code, now, store, default):
        return self.bars, 0.8, "typical"

    def release_unused(self, keep):
        self.released = keep


class Converter(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.store = CandleStore(Path(self.dir.name) / "common.sqlite3")

    def tearDown(self):
        self.store.close()
        self.dir.cleanup()

    def test_converted_bars_are_pushed_with_their_source(self):
        now = datetime(2026, 10, 19, 15, 0, tzinfo=timezone.utc)
        feed = FakeFeed(frame([("2026-10-19 10:00", 10, 10.5, 9.9, 10.2, 1000)]))
        self.assertEqual(converter.convert_hot(self.store, feed, ["AAA"], now, 0.8), {"AAA": 1})
        self.assertEqual(self.store.db.execute("SELECT source FROM bars WHERE symbol='AAA'").fetchone()[0], "moomoo")

    def test_a_moomoo_bar_keeps_its_prices_when_yahoo_writes_the_same_bar_later(self):
        t = "2026-10-19 10:00"
        ms = int(pd.Timestamp(t, tz=NY).timestamp() * 1000)
        self.store.upsert_frame("AAA", frame([(t, 10, 11, 9, 10.5, 1000)]), "moomoo", now_ms=ms + 20 * 60_000)
        self.store.upsert_frame("AAA", frame([(t, 10, 11, 9, 10.1, 400)]), "yahoo", now_ms=ms + 20 * 60_000)
        self.assertEqual(self.store.db.execute("SELECT close FROM bars").fetchone()[0], 10.5)
        self.store.upsert_frame("AAA", frame([(t, 10, 11, 9, 10.1, 900)]), "yahoo", now_ms=ms + 3 * 3600_000)  # hours later: still Moomoo's prices, now both volumes
        self.assertEqual(self.store.db.execute("SELECT close, source, volume FROM bars").fetchone(), (10.5, "moomoo", 950.0))

    def test_hot_requests_expire_and_the_plans_candidates_come_first(self):
        self.store.request_hot(["BBB", "CCC"], "confirm", 1_000_000)
        self.assertEqual(converter.hot_list(self.store, ["US.AAA", "BBB"], 1_000_000 + 1000), ["AAA", "BBB", "CCC"])
        self.assertEqual(converter.hot_list(self.store, ["AAA", "BBB"], 1_000_000 + 1000, priority=["TRADED"]), ["TRADED", "AAA", "BBB", "CCC"])
        self.assertEqual(converter.hot_list(self.store, [], 1_000_000 + 700_000), [])

    def test_a_new_candidate_is_fetched_once_and_the_others_stay_subscribed(self):
        now = datetime(2026, 10, 19, 15, 0, tzinfo=timezone.utc)
        feed = FakeFeed(frame([("2026-10-19 10:00", 10, 10.5, 9.9, 10.2, 1000)]))
        converter.convert_hot(self.store, feed, ["NEW"], now, 0.8, keep=["OLD", "NEW"])
        self.assertEqual(feed.released, {"US.OLD", "US.NEW"}, "the seed does not drop the candidates already subscribed")
        self.assertTrue(feed.pushed, "and the subscription is a push subscription")

    def test_pushed_candle_updates_are_converted_and_stored_for_candidates_only(self):
        now = datetime(2026, 10, 19, 15, 0, tzinfo=timezone.utc)
        data = pd.DataFrame([{"code": "US.AAA", "time_key": "2026-10-19 10:15:00", "open": 10.0, "close": 10.2, "high": 10.3, "low": 9.9, "volume": 800.0, "k_type": "K_1M"},
                             {"code": "US.OTHER", "time_key": "2026-10-19 10:15:00", "open": 5.0, "close": 5.1, "high": 5.2, "low": 4.9, "volume": 100.0, "k_type": "K_1M"},
                             {"code": "US.AAA", "time_key": "2026-10-19 10:15:00", "open": 1.0, "close": 1.0, "high": 1.0, "low": 1.0, "volume": 1.0, "k_type": "K_15M"}])
        self.store.set_scale("AAA", 0.8, "t")
        written = converter.apply_push(self.store, data, {"US.AAA"}, 0.8, now)
        self.assertEqual(written, {"AAA": 1}, "the candidate's 1-minute update only")
        row = self.store.db.execute("SELECT symbol, close, volume, source FROM bars").fetchall()
        self.assertEqual(row, [("AAA", 10.2, 1000.0, "moomoo")], "converted with the symbol's volume scale (800 / 0.8)")

    def test_every_candidate_push_and_every_log_line_is_kept_in_a_database(self):
        import logging
        from papercompmoomoo import dblog
        db_path = Path(self.dir.name) / "feed.sqlite3"
        data = pd.DataFrame([{"code": "US.AAA", "time_key": "2026-10-19 10:15:00", "open": 10.0, "close": 10.2, "high": 10.3, "low": 9.9, "volume": 800.0, "k_type": "K_1M"}])
        self.assertEqual(converter.record_pushes(data, 123, db_path), 1)
        self.assertEqual(sqlite3.connect(db_path).execute("SELECT recv_ms, code, close FROM pushes").fetchall(), [(123, "US.AAA", 10.2)])
        handler = dblog.DbLogHandler("test", Path(self.dir.name) / "logs.sqlite3")
        handler.setFormatter(logging.Formatter("%(message)s"))
        log = logging.getLogger("papercompmoomoo.test")
        log.addHandler(handler)
        log.setLevel(logging.INFO)
        log.warning("an exit was sent for %s", "AAA")
        logging.getLogger("yfinance").addHandler(handler)
        logging.getLogger("yfinance").error("noise")
        log.removeHandler(handler)
        logging.getLogger("yfinance").removeHandler(handler)
        self.assertEqual(handler.db.execute("SELECT source, level, message FROM logs").fetchall(), [("test", "WARNING", "an exit was sent for AAA")], "stored, and the noisy library is not")

    def test_moomoo_candles_are_downloaded_for_the_locked_plans_candidates_only(self):
        import json
        plan = Path(self.dir.name) / "plan.json"
        plan.write_text(json.dumps({"locked": True, "candidates": [{"symbol": "US.AAA"}, {"symbol": "US.BBB"}]}))
        self.assertEqual(converter.candidate_symbols(plan), ["AAA", "BBB"])
        plan.write_text(json.dumps({"locked": False, "candidates": [{"symbol": "US.AAA"}]}))
        self.assertEqual(converter.candidate_symbols(plan), [], "an unlocked plan has no candidates yet")
        self.assertEqual(converter.candidate_symbols(Path(self.dir.name) / "none.json"), [])
        self.assertFalse(hasattr(converter, "held_symbols"), "a held position that is not a candidate gets no candle download")

    def test_in_the_live_window_only_the_candidates_download_and_everything_else_waits_for_the_close(self):
        from zoneinfo import ZoneInfo
        ny = ZoneInfo("America/New_York")
        at = lambda d, h, m=0: datetime(2026, 10, d, h, m, tzinfo=ny).astimezone(timezone.utc)
        steps = lambda now, ins, done=False: converter.round_steps(now, ins, 0.0, 10_000.0, done)
        pre = steps(at(21, 8), False)  # Wednesday 08:00, the pre-market orders are live
        self.assertEqual((pre["hot"], pre["charts"], pre["history"], pre["yahoo_in_session"], pre["yahoo_catch_up"]), (True, True, False, False, False))
        mid = steps(at(21, 12), True)  # mid-session: the old minute Yahoo feed stays paused
        self.assertEqual((mid["hot"], mid["charts"], mid["history"], mid["yahoo_in_session"], mid["yahoo_catch_up"]), (True, True, False, False, False))
        after = steps(at(21, 17), False)  # after the close: the bulk work catches up, once
        self.assertEqual((after["hot"], after["charts"], after["history"], after["yahoo_catch_up"]), (False, False, True, True))
        self.assertFalse(steps(at(21, 17), False, done=True)["yahoo_catch_up"], "once a day")
        early = steps(at(21, 6), False)
        self.assertEqual((early["hot"], early["charts"], early["history"], early["yahoo_catch_up"]), (False, True, False, False))
        weekend = steps(at(24, 12), False)  # Saturday
        self.assertEqual((weekend["hot"], weekend["charts"], weekend["history"], weekend["yahoo_catch_up"]), (False, False, True, False))

    def test_the_old_store_is_copied_once(self):
        d = Path(self.dir.name)
        old = sqlite3.connect(d / "old.sqlite3")
        old.execute("CREATE TABLE t(x)")
        old.execute("INSERT INTO t VALUES(1)")
        old.commit()
        old.close()
        self.assertTrue(converter.migrate_legacy(d / "new.sqlite3", d / "old.sqlite3"))
        self.assertFalse(converter.migrate_legacy(d / "new.sqlite3", d / "old.sqlite3"))
        self.assertEqual(sqlite3.connect(d / "new.sqlite3").execute("SELECT x FROM t").fetchone()[0], 1)

    def test_heartbeat_merges_fields(self):
        p = Path(self.dir.name) / "s.json"
        converter.write_status(p, yahoo_exit=0)
        converter.write_status(p, moomoo_bars=3)
        d = json.loads(p.read_text())
        self.assertEqual((d["yahoo_exit"], d["moomoo_bars"]), (0, 3))

    def test_a_heartbeat_thread_keeps_the_file_fresh_while_the_main_loop_is_busy_and_keeps_the_other_fields(self):
        import time
        p = Path(self.dir.name) / "hb.json"
        converter.write_status(p, yahoo_exit=0)
        first = json.loads(p.read_text())["time"]
        converter.start_heartbeat(p, interval=0.05)
        time.sleep(1.2)  # the stamp has a one-second resolution
        d = json.loads(p.read_text())
        self.assertGreater(d["time"], first, "refreshed with nobody calling write_status")
        self.assertEqual(d["yahoo_exit"], 0, "the other fields are kept")
        # writes from the main loop and from the thread at the same time lose nothing
        for i in range(50):
            converter.write_status(p, moomoo_bars=i)
        self.assertEqual(json.loads(p.read_text())["moomoo_bars"], 49)


if __name__ == "__main__":
    unittest.main()
