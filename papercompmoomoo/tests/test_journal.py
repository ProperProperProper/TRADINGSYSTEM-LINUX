import tempfile
import unittest
from pathlib import Path

from papercompmoomoo.journal import Journal

PROFILE = {"last_bar_volume": 5000.0, "typical_bar_volume": 4000.0, "median_range": 0.004}


class JournalTests(unittest.TestCase):
    def setUp(self):
        self.journal = Journal(Path(tempfile.mkdtemp()) / "j.sqlite3")
        self.addCleanup(self.journal.close)

    def test_a_signal_is_recorded_with_its_numbers_and_the_decision_can_be_updated(self):
        sid = self.journal.signal(1_000, "US.AAA", 900, 10.5, 6.2, 3.4, PROFILE, 1e6, True, "candidate")
        self.journal.decide(sid, "ordered")
        row = self.journal.db.execute("SELECT symbol, price, rvol, move_pct, last_bar_volume, typical_bar_volume, median_range, nasdaq, decision FROM signals WHERE id=?", (sid,)).fetchone()
        self.assertEqual(row, ("US.AAA", 10.5, 6.2, 3.4, 5000.0, 4000.0, 0.004, 1, "ordered"))

    def test_a_trade_carries_the_signal_behind_its_entry(self):
        sid = self.journal.signal(1_000, "US.AAA", 900, 10.5, 6.2, 3.4, PROFILE, 1e6, True, "ordered")
        self.journal.order(1_001, "US.AAA", "BUY", 100, sid, "42", 10.5, 6.2, 3.4, "money")
        self.journal.order(9_000, "US.AAA", "SELL", 100, None, "", 10.9, None, None, "stop")
        self.journal.trade("US.AAA", 1_001, 9_000, 100, 10.5, 10.9, 3.8, "trailing stop")
        row = self.journal.db.execute("SELECT symbol, pnl_pct, reason, signal_id, rvol, move_pct FROM trades").fetchone()
        self.assertEqual(row, ("US.AAA", 3.8, "trailing stop", sid, 6.2, 3.4))

    def test_a_trade_for_a_symbol_the_journal_never_saw_is_still_recorded(self):
        self.journal.trade("US.ZZZ", 1, 2, 5, 1.0, 1.1, 10.0, "time limit")
        self.assertEqual(self.journal.db.execute("SELECT signal_id, rvol FROM trades").fetchone(), (None, None))

    def test_ids_only_grow_so_a_reader_can_continue_from_the_last_one_it_saw(self):
        ids = [self.journal.signal(i, "US.A", i, 1.0, 1.0, 1.0, PROFILE, 1.0, True, "below_trigger") for i in range(3)]
        self.assertEqual(ids, sorted(ids))
        self.assertEqual(len(set(ids)), 3)


if __name__ == "__main__":
    unittest.main()
