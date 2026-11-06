import tempfile
import unittest
import json
import subprocess
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

from papercompmoomoo import planner
from papercompmoomoo.signals import OWN_REMARK, PRE_OPEN_REMARK, orders_to_cancel

NY = ZoneInfo("America/New_York")


def ny(y, m, d, h, mi=0):
    return datetime(y, m, d, h, mi, tzinfo=NY).astimezone(timezone.utc)


class Planner(unittest.TestCase):
    def momentum(self, cutoff=20748, candidates=None, **updates):
        value = {"schema": 1, "strategy": planner.MOMENTUM_STRATEGY, "data_cutoff_day": cutoff, "lookback_sessions": 20,
                 "candidates": candidates or [{"symbol": "AAA", "score": 0.2, "observations": 20}, {"symbol": "BBB", "score": 0.1, "observations": 20}]}
        value.update(updates)
        return json.dumps(value)

    def test_momentum_selection_is_causal_ranked_and_filtered_to_tradable_symbols(self):
        parsed = planner.parse_momentum_selection(self.momentum(), "2026-10-23", 1, {"US.BBB"})
        self.assertEqual(parsed["data_cutoff"], "2026-10-22")
        self.assertEqual(parsed["candidates"], [{"symbol": "US.BBB", "score": 0.1, "observations": 20, "rank": 2}])

    def test_momentum_selection_refuses_wrong_stale_future_incomplete_and_duplicate_output(self):
        tradable = {"US.AAA", "US.BBB"}
        bad = [
            self.momentum(schema=2), self.momentum(cutoff=20740), self.momentum(cutoff=20749),
            self.momentum(candidates=[{"symbol": "AAA", "score": 0.2, "observations": 19}]),
            self.momentum(candidates=[{"symbol": "AAA", "score": 0.2, "observations": 20}, {"symbol": "AAA", "score": 0.1, "observations": 20}]),
        ]
        for raw in bad:
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                planner.parse_momentum_selection(raw, "2026-10-23", 1, tradable)

    def tuned(self, candidates, **updates):
        value = {"schema": 1, "strategy": planner.TUNED_STRATEGY, "data_cutoff_day": 20748, "lookback_sessions": 0, "candidates": candidates}
        value.update(updates)
        return json.dumps(value)

    def test_tuned_selection_allows_fewer_than_plan_size_but_each_candidate_needs_its_own_lookback(self):
        good = self.tuned([{"symbol": "AAA", "score": 0.01, "observations": 12, "lookback": 12}])
        parsed = planner.parse_momentum_selection(good, "2026-10-23", 15, {"US.AAA"})
        self.assertEqual(parsed["strategy"], planner.TUNED_STRATEGY)
        self.assertEqual(parsed["candidates"][0]["observations"], 12)
        bad = [
            self.tuned([]),  # nothing accepted today: no plan, never a fallback
            self.tuned([{"symbol": "AAA", "score": 0.01, "observations": 12, "lookback": 11}]),
            self.tuned([{"symbol": "AAA", "score": 0.01, "observations": 31, "lookback": 31}]),
            self.tuned([{"symbol": "AAA", "score": 0.01, "observations": 12, "lookback": 12}], lookback_sessions=20),
            self.tuned([{"symbol": "AAA", "score": 0.01, "observations": 12, "lookback": 12}], data_cutoff_day=20749),
        ]
        for raw in bad:
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                planner.parse_momentum_selection(raw, "2026-10-23", 15, {"US.AAA"})

    def test_momentum_command_failure_is_a_refusal_not_a_fallback_strategy(self):
        failed = lambda *a, **k: subprocess.CompletedProcess(a, 3, stdout="", stderr="broken")
        with self.assertRaisesRegex(ValueError, "broken"):
            planner.momentum_selection("2026-10-23", 1, {"US.AAA"}, run=failed)

    def test_the_market_phase_comes_from_opend_and_the_clock_only_when_opend_cannot_say(self):
        now = ny(2026, 10, 21, 10)
        self.assertEqual(planner.phase_from_state("MORNING", now), "open")
        self.assertEqual(planner.phase_from_state("PRE_MARKET_BEGIN", now), "pre")
        self.assertEqual(planner.phase_from_state("AFTER_HOURS_BEGIN", now), "after")
        self.assertEqual(planner.phase_from_state("CLOSED", now), "closed")
        self.assertEqual(planner.phase_from_state("MarketState.MORNING", now), "open", "the enum's text form")
        self.assertEqual(planner.phase_from_state(None, ny(2026, 10, 21, 10)), "open")
        self.assertEqual(planner.phase_from_state(None, ny(2026, 10, 21, 7)), "pre")
        self.assertEqual(planner.phase_from_state(None, ny(2026, 10, 21, 17)), "after")
        self.assertEqual(planner.phase_from_state(None, ny(2026, 10, 24, 11)), "closed", "a Saturday")

    def test_the_next_open_skips_weekends_follows_daylight_saving_and_the_lead_window_starts_two_hours_before(self):
        self.assertEqual(planner.next_open(ny(2026, 10, 21, 17)), ny(2026, 10, 22, 9, 30))
        self.assertEqual(planner.next_open(ny(2026, 10, 21, 7)), ny(2026, 10, 21, 9, 30), "before the open: today's")
        self.assertEqual(planner.next_open(ny(2026, 10, 23, 17)), ny(2026, 10, 26, 9, 30), "Friday evening: Monday")
        self.assertEqual(planner.next_open(ny(2026, 10, 24, 12)), ny(2026, 10, 26, 9, 30))
        summer, winter = planner.next_open(ny(2026, 10, 21, 17)), planner.next_open(ny(2026, 11, 3, 17))
        self.assertEqual((summer.hour, winter.hour), (13, 14), "13:30 UTC in summer time, 14:30 UTC after the clocks change")
        open_utc = ny(2026, 10, 22, 9, 30)
        self.assertTrue(planner.in_lead_window(ny(2026, 10, 22, 7, 30), open_utc, 2.0))
        self.assertFalse(planner.in_lead_window(ny(2026, 10, 22, 7, 29), open_utc, 2.0))
        self.assertFalse(planner.in_lead_window(open_utc, open_utc, 2.0))

    def test_candidates_are_searched_only_after_the_close_data_is_published(self):
        self.assertFalse(planner.after_close_ready(ny(2026, 10, 21, 16, 5)))
        self.assertTrue(planner.after_close_ready(ny(2026, 10, 21, 16, 20)))
        self.assertTrue(planner.after_close_ready(ny(2026, 10, 21, 23, 0)))
        self.assertFalse(planner.after_close_ready(ny(2026, 10, 21, 12, 0)), "the session is still running")

    def test_a_plan_is_kept_on_disk_and_a_refusal_is_final_only_when_retrying_cannot_help(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "plan.json"
            plan = planner.new_plan("2026-10-22", [{"symbol": "US.A", "why": "w", "terms": {"stop_pct": 2.0}, "score": 3.0}], 1, "after_close")
            planner.save_plan(plan, path)
            self.assertEqual(planner.load_plan(path)["candidates"][0]["status"], planner.PENDING)
            self.assertIsNone(planner.load_plan(Path(d) / "none.json"))
        self.assertTrue(planner.final_refusal("the order failed: Trading of stocks and options from companies with market caps below US$500 million is unavailable."), "retrying cannot help")
        self.assertTrue(planner.final_refusal("already held"))
        self.assertTrue(planner.final_refusal("15 positions or working orders already (maximum 15)"))
        self.assertFalse(planner.final_refusal("the simulated account cannot be read"))
        self.assertFalse(planner.final_refusal("the order failed: timeout"))

    def test_a_pre_market_order_may_wait_for_the_open_but_any_other_old_order_is_cancelled(self):
        now = datetime(2026, 10, 21, 12, 0, tzinfo=NY)
        rows = [{"order_id": 1, "code": "US.A", "remark": PRE_OPEN_REMARK, "created": now - timedelta(hours=2)},
                {"order_id": 2, "code": "US.B", "remark": OWN_REMARK + " momentum entry", "created": now - timedelta(minutes=10)},
                {"order_id": 3, "code": "US.C", "remark": PRE_OPEN_REMARK, "created": now - timedelta(hours=5)}]
        self.assertEqual(sorted(r["order_id"] for r in orders_to_cancel(rows, now)), [2, 3])

    def test_a_restart_keeps_the_plans_pre_market_orders_but_cancels_any_other_left_over_order(self):
        now = datetime(2026, 10, 21, 12, 0, tzinfo=NY)
        rows = [{"order_id": 1, "code": "US.A", "remark": PRE_OPEN_REMARK, "created": now - timedelta(minutes=3)},
                {"order_id": 2, "code": "US.B", "remark": OWN_REMARK + " momentum entry", "created": now - timedelta(minutes=1)}]
        self.assertEqual([r["order_id"] for r in orders_to_cancel(rows, now, at_startup=True)], [2])


if __name__ == "__main__":
    unittest.main()
