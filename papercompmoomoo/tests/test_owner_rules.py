import tempfile
import unittest
from pathlib import Path

from papercompmoomoo import board, council, owner_rules as o, rules

DAY = 86_400_000
T0 = 1_800_000_000_000


class Guard(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        d = Path(self.dir.name)
        self.guard = o.CapitalGuard(d / "g.json", d / "resume")

    def test_paper_mode_records_daily_loss_without_blocking_simulated_orders(self):
        self.assertEqual(self.guard.update(100_000.0, T0), [])
        self.assertTrue(self.guard.allow_new_orders()[0])
        self.assertEqual(self.guard.update(99_000.0, T0 + 60_000), [])
        news = self.guard.update(97_900.0, T0 + 120_000)
        self.assertEqual(news, [], "no loss halt emitted")
        self.assertTrue(self.guard.allow_new_orders()[0])
        self.assertEqual(self.guard.update(97_800.0, T0 + 180_000), [])
        self.guard.update(97_800.0, T0 + DAY)
        self.assertTrue(self.guard.allow_new_orders()[0])

    def test_paper_mode_records_peak_drawdown_without_blocking_simulated_orders(self):
        self.guard.update(100_000.0, T0)
        self.guard.update(110_000.0, T0 + DAY)
        news = self.guard.update(104_000.0, T0 + 2 * DAY)
        self.assertEqual(news, [], "no drawdown halt emitted")
        self.guard.update(104_000.0, T0 + 3 * DAY)
        self.assertTrue(self.guard.allow_new_orders()[0])
        again = o.CapitalGuard(self.guard.path, self.guard.resume_path)
        self.assertTrue(again.allow_new_orders()[0])
        news = self.guard.update(104_500.0, T0 + 3 * DAY + 1000)
        self.assertTrue(self.guard.allow_new_orders()[0])

    def test_wording_that_would_loosen_a_hard_rule_is_refused(self):
        for text in ("Raise the leverage to 5x for strong setups.", "Remove the stop on volatile names.", "Double down when a position is losing.", "Average down on dips.",
                     "Use the real account for the live test.", "Ignore the drawdown limit this week.", "Go all-in on the best candidate.", "Disable the circuit breaker."):
            self.assertIsNotNone(o.check_text(text), text)
        for text in ("Prefer trailing stops of 1.5-3%.", "Do not pick companies under US$500 million.", "Tighten the stop to 2%."):
            self.assertIsNone(o.check_text(text), text)

    def test_the_board_the_rules_and_the_council_all_refuse_a_loosening_proposal(self):
        db = board.connect(Path(self.dir.name) / "b.sqlite3")
        self.addCleanup(db.close)
        with self.assertRaises(ValueError) as caught:
            rules.add_rule(db, "planner", "advice", "Raise the leverage when sure.", "a good week")
        self.assertIn("hard rules", str(caught.exception))
        with self.assertRaises(ValueError):
            council.open_decision(db, "commander", "More leverage", "Allow 4x leverage for the plan.", payload={"op": "none"})
        self.assertTrue(o.pin_hard_rules(db))
        pinned = board.recent(db)[0]
        self.assertEqual((pinned["sender"], pinned["topic"]), ("owner", "hard rules"))
        self.assertIn("wellbeing", pinned["body"])

    def test_the_numbers_of_the_hard_rules_are_what_the_text_says(self):
        self.assertEqual((o.MAX_LEVERAGE, o.MAX_POWER_USE, o.DAILY_LOSS_LIMIT_PCT, o.PEAK_DRAWDOWN_LIMIT_PCT), (2.0, 0.8, 2.0, 5.0))
        self.assertIn("2x", o.HARD_TEXT)
        self.assertIn("80%", o.HARD_TEXT)


if __name__ == "__main__":
    unittest.main()
