import unittest

import pandas as pd

from papercompmoomoo.signals import average_daily_volume, closed_bars, time_matched_relative_volume


def session(day: str, volumes, close_start: float = 100.0, bars_from: str = "09:30"):
    index = pd.date_range(f"{day} {bars_from}", periods=len(volumes), freq="1min", tz="America/New_York", name="Datetime")
    return pd.DataFrame({"Close": [close_start + i * 0.1 for i in range(len(volumes))], "Volume": volumes}, index=index)


class RelativeVolume(unittest.TestCase):
    def test_today_against_the_same_time_of_day_on_earlier_sessions(self):
        usual = [1000, 500, 400, 300, 300, 300]
        frame = pd.concat([session("2026-10-14", usual), session("2026-10-15", usual), session("2026-10-16", usual, 200.0),
                           session("2026-10-19", [5000, 2500, 2000])])
        rvol, previous_close = time_matched_relative_volume(frame)
        self.assertAlmostEqual(rvol, (5000 + 2500 + 2000) / (1000 + 500 + 400), places=9)  # 5.0: ten thousand five hundred over 1900
        self.assertAlmostEqual(previous_close, 200.5, places=9)

    def test_a_whole_five_day_volume_over_one_day_is_no_longer_what_counts(self):
        usual = [1000] * 26
        frame = pd.concat([session(d, usual) for d in ["2026-10-13", "2026-10-14", "2026-10-15", "2026-10-16"]] + [session("2026-10-19", [1000] * 4)])
        rvol, _ = time_matched_relative_volume(frame)
        self.assertAlmostEqual(rvol, 1.0, places=9)  # a normal morning is 1x, not "5x" as the old 5-day-over-one-day figure said

    def test_a_slot_with_no_trade_counts_as_zero_volume_and_the_session_is_kept(self):
        full = [1000] * 6
        thin = session("2026-10-15", [1000, 1000, 1000])  # nothing traded in the last three slots: no bars, not a gap in the data
        frame = pd.concat([session("2026-10-14", full), thin, session("2026-10-16", full), session("2026-10-19", [2000] * 6)])
        rvol, _ = time_matched_relative_volume(frame)
        self.assertAlmostEqual(rvol, 12000 / ((6000 + 3000 + 6000) / 3), places=9)

    def test_only_a_half_day_that_was_already_over_is_left_out(self):
        full = [1000] * 20
        frame = pd.concat([session("2026-11-25", full), session("2026-11-27", [5000] * 10),  # the day after Thanksgiving closes at 13:00
                           session("2026-11-30", full), session("2026-12-01", [2000] * 20)])
        rvol, _ = time_matched_relative_volume(frame)
        self.assertAlmostEqual(rvol, 2.0, places=9, msg="the half day is not comparable at 14:15 and is left out")
        morning = pd.concat([session("2026-11-25", full), session("2026-11-27", [1000] * 10), session("2026-11-30", full), session("2026-12-01", [2000] * 5)])
        self.assertAlmostEqual(time_matched_relative_volume(morning)[0], 2.0, places=9, msg="before 13:00 the half day is a normal comparison")

    def test_too_little_history_or_no_volume_gives_none_and_an_empty_frame_gives_nothing(self):
        one_prior = pd.concat([session("2026-10-16", [1000] * 4), session("2026-10-19", [3000] * 4)])
        self.assertEqual(time_matched_relative_volume(one_prior)[0], None)
        self.assertEqual(time_matched_relative_volume(one_prior)[1], 100.3 + 0.0)
        silent = pd.concat([session("2026-10-14", [0] * 4), session("2026-10-15", [0] * 4), session("2026-10-19", [10] * 4)])
        self.assertIsNone(time_matched_relative_volume(silent)[0])
        self.assertEqual(time_matched_relative_volume(None), (None, None))
        self.assertEqual(time_matched_relative_volume(one_prior.iloc[0:0]), (None, None))

    def test_a_bar_still_in_progress_can_only_lower_the_figure(self):
        usual = [1000] * 6
        base = [session("2026-10-14", usual), session("2026-10-15", usual)]
        done = time_matched_relative_volume(pd.concat(base + [session("2026-10-19", [3000, 3000, 3000])]))[0]
        partial = time_matched_relative_volume(pd.concat(base + [session("2026-10-19", [3000, 3000, 400])]))[0]
        self.assertLess(partial, done)


class NoLookAhead(unittest.TestCase):
    def test_a_signal_at_a_given_moment_ignores_every_bar_after_it(self):
        import numpy as np
        from datetime import datetime
        from zoneinfo import ZoneInfo

        rng = np.random.default_rng(3)
        days = ["2026-10-12", "2026-10-13", "2026-10-14", "2026-10-15", "2026-10-16", "2026-10-19"]
        full = pd.concat([session(d, list(rng.uniform(500, 2000, 26)), close_start=10 + i) for i, d in enumerate(days)])
        for hour, minute in ((10, 31), (11, 1), (13, 46), (15, 40)):
            now = datetime(2026, 10, 19, hour, minute, tzinfo=ZoneInfo("America/New_York"))
            honest = closed_bars(full, now)
            scrambled = full.copy()
            later = scrambled.index + pd.Timedelta(minutes=1) > pd.Timestamp(now)
            scrambled.loc[later, "Volume"] = scrambled.loc[later, "Volume"] * 50
            scrambled.loc[later, "Close"] = scrambled.loc[later, "Close"] * 3
            self.assertEqual(time_matched_relative_volume(closed_bars(scrambled, now)), time_matched_relative_volume(honest), f"at {hour}:{minute}")
            self.assertTrue((closed_bars(scrambled, now).index <= pd.Timestamp(now) - pd.Timedelta(minutes=1)).all(), "no unfinished bar is ever used")


class Baseline(unittest.TestCase):
    def test_a_short_history_still_gives_a_baseline_when_a_minimum_is_asked_for_and_none_otherwise(self):
        frame = pd.concat([session("2026-10-15", [100.0] * 4), session("2026-10-16", [300.0] * 4)])
        today = pd.Timestamp("2026-10-19").date()
        self.assertIsNone(average_daily_volume(frame, 21, today=today), "the strict default still needs 21 sessions")
        self.assertEqual(average_daily_volume(frame, 21, today=today, minimum=1), 800.0, "mean of 400 and 1200")
        self.assertIsNone(average_daily_volume(frame.iloc[:0], 21, today=today, minimum=1))


if __name__ == "__main__":
    unittest.main()
