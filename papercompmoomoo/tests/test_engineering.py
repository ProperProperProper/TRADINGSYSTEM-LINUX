import json
import tempfile
import unittest
from pathlib import Path

from papercompmoomoo import board, council, engineering as g

NOW = 1_000_000_000


class Done:
    def __init__(self, out):
        self.stdout, self.stderr = out, ""


class Starting(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.db = board.connect(Path(self.dir.name) / "b.sqlite3")
        self.addCleanup(self.db.close)
        self.calls = []

        def run(cmd, **kw):
            self.calls.append(cmd)
            return Done(json.dumps({"ok": True, "why": "committed", "files": ["src/a.py"]}))

        self.run = run

    def tick(self, now=NOW, state=None, local_ok=True):
        return g.tick(self.db, now, state or {}, {"claude_daily_calls": 0, "codex_daily_calls": 0}, local_ok, run=self.run)

    def test_only_the_owners_messages_to_engineer_start_a_job_and_each_is_read_once(self):
        board.post(self.db, "judge", "please change the stop", to="engineer", now_ms=NOW)
        self.assertEqual(g.from_board(self.db, NOW), 0, "an agent's message is not an order")
        board.post(self.db, "owner", "add a test for the sizing", to="engineer", now_ms=NOW)
        self.assertEqual(g.from_board(self.db, NOW), 1)
        self.assertEqual(g.from_board(self.db, NOW), 0, "already read")

    def test_loosening_wording_and_duplicates_are_not_queued(self):
        self.assertIsNone(g.queue(self.db, "council", "raise the leverage to 5x and remove the stop loss", NOW))
        first = g.queue(self.db, "council", "add a test for planner", NOW)
        self.assertIsNotNone(first)
        self.assertIsNone(g.queue(self.db, "council", "add a test for planner", NOW), "the same task is not queued twice")
        self.assertIsNone(g.queue(self.db, "council", "   ", NOW))

    def test_a_job_runs_through_the_gate_with_the_push_flag_and_is_told_on_the_board(self):
        g.queue(self.db, "owner", "add a test for planner", NOW)
        result = self.tick()
        self.assertEqual((result["ok"], result["agent"]), (True, result["agent"]))
        self.assertIn("papercompmoomoo.engineer", self.calls[0])
        self.assertEqual(self.calls[0][-1], "--push")
        self.assertEqual(self.db.execute("SELECT status FROM engineering_jobs").fetchone()[0], "done")
        self.assertTrue([m for m in board.recent(self.db) if m["topic"] == "engineering"])

    def test_at_most_one_job_an_hour_and_one_at_a_time(self):
        g.queue(self.db, "owner", "task one", NOW)
        g.queue(self.db, "owner", "task two", NOW)
        self.assertIsNotNone(self.tick())
        self.assertIsNone(self.tick(NOW + 60_000), "the hour's gap has not passed")
        self.assertIsNotNone(self.tick(NOW + g.MIN_GAP_MS + 1))
        self.assertEqual(len(self.calls), 2)

    def test_the_local_model_is_primary_and_other_backends_are_fallback(self):
        state = {"claude": {"limited_until_ms": NOW + 10_000}, "codex": {"limited_until_ms": NOW + 10_000}}
        self.assertEqual(g.pick_agent(state, {}, NOW, True), "local")
        self.assertIsNone(g.pick_agent(state, {}, NOW, False))
        self.assertEqual(g.pick_agent({}, {"claude_daily_calls": 0, "codex_daily_calls": 0}, NOW, True), "local")

    def test_a_refused_or_crashing_job_is_a_result_not_a_crash(self):
        g.queue(self.db, "owner", "task", NOW)
        self.run = lambda cmd, **kw: Done(json.dumps({"ok": False, "why": "the tests fail", "files": []}))
        self.assertFalse(self.tick()["ok"])
        self.assertEqual(self.db.execute("SELECT status FROM engineering_jobs").fetchone()[0], "refused")
        g.queue(self.db, "owner", "task two", NOW)

        def boom(cmd, **kw):
            raise OSError("no python")

        self.run = boom
        self.assertFalse(self.tick(NOW + g.MIN_GAP_MS + 1)["ok"])

    def test_an_approved_council_decision_with_the_engineer_operation_queues_a_job(self):
        self.assertIn("queued", council.apply_payload(self.db, {"op": "engineer", "task": "add a test for sizing"}, NOW))
        self.assertIn("refused", council.apply_payload(self.db, {"op": "engineer", "task": "remove the stop loss"}, NOW))
        self.assertIn("engineer", council.OPS)


if __name__ == "__main__":
    unittest.main()
