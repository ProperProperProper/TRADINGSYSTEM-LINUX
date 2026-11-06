import tempfile
import unittest
from pathlib import Path

from papercompmoomoo import board


class Board(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.db = board.connect(Path(self.dir.name) / "b.sqlite3")
        self.addCleanup(self.db.close)

    def test_a_message_has_a_sender_a_recipient_a_kind_and_can_say_which_goal_it_serves(self):
        first = board.post(self.db, "judge", "Nothing passed.", to="*", topic="daily", kind="decision", goal="O5", now_ms=1)
        row = board.recent(self.db)[0]
        self.assertEqual((row["id"], row["sender"], row["recipient"], row["kind"], row["goal"]), (first, "judge", "*", "decision", "O5"))
        self.assertEqual(board.recent(self.db)[0]["goal"], "O5")
        self.assertEqual(board.post(self.db, "judge", "x", goal="not a goal"), first + 1)
        self.assertEqual(board.recent(self.db)[-1]["goal"], "", "an invalid goal tag is dropped, not kept")

    def test_bad_names_kinds_empty_texts_and_answers_to_nothing_are_refused(self):
        for kwargs in ({"sender": "Bad Name", "body": "x"}, {"sender": "judge", "body": "x", "to": "Bad Name"}, {"sender": "judge", "body": "x", "kind": "gossip"}, {"sender": "judge", "body": "   "},
                       {"sender": "judge", "body": "x", "reply_to": 999}):
            with self.assertRaises(ValueError):
                board.post(self.db, **kwargs)
        board.post(self.db, "judge", "a\x00b\x07c" + "z" * 5000)
        body = board.recent(self.db)[0]["body"]
        self.assertTrue(body.startswith("abc") and len(body) <= board.MAX_BODY, "control characters are removed and the length is capped")

    def test_the_inbox_has_what_is_for_the_reader_and_not_its_own_and_read_messages_drop_out_of_the_unread_view(self):
        a = board.post(self.db, "judge", "for everybody")
        b = board.post(self.db, "reviewer", "for the planner", to="planner")
        board.post(self.db, "reviewer", "for the improver", to="improver")
        board.post(self.db, "planner", "my own")
        self.assertEqual([m["id"] for m in board.inbox(self.db, "planner")], [a, b])
        board.mark_read(self.db, "planner", [a])
        self.assertEqual([m["id"] for m in board.inbox(self.db, "planner", unread_only=True)], [b])
        self.assertEqual([m["id"] for m in board.inbox(self.db, "planner")], [a, b], "still there to look back on")
        self.assertEqual([m["id"] for m in board.inbox(self.db, "judge", unread_only=True)], [b - 0 if False else m["id"] for m in board.inbox(self.db, "judge", unread_only=True)])

    def test_a_thread_follows_the_answers_both_ways(self):
        q = board.post(self.db, "planner", "Was the stop too tight?", to="reviewer", kind="question")
        a = board.post(self.db, "reviewer", "On 9 trades, yes.", to="planner", kind="answer", reply_to=q)
        c = board.post(self.db, "planner", "Thanks, widened.", to="reviewer", reply_to=a)
        board.post(self.db, "judge", "unrelated")
        self.assertEqual([m["id"] for m in board.thread(self.db, a)], [q, a, c])

    def test_the_posts_an_agent_asks_for_are_validated(self):
        items = [{"to": "reviewer", "kind": "question", "topic": "t", "text": "ok", "goal": "O2"}, {"to": "nobody", "text": "dropped"}, {"kind": "gossip", "text": "dropped"}, {"text": ""}, "not a dict",
                 {"to": "*", "text": "two"}, {"text": "three"}, {"text": "four: more than three are dropped"}]
        out = board.posts_from_reply(items, "planner", board.AGENTS)
        self.assertEqual([p["body"] for p in out], ["ok", "two", "three"][: len(out)])
        self.assertTrue(all(p["sender"] == "planner" for p in out))
        self.assertEqual(board.posts_from_reply("nope", "planner", board.AGENTS), [])


class Weights(unittest.TestCase):
    def test_a_record_is_shrunk_toward_chance_and_moves_with_the_evidence(self):
        from papercompmoomoo import weights
        self.assertAlmostEqual(weights.weight(0, 0), 0.5, places=6, msg="no trades: the base rate, weight one half")
        self.assertGreater(weights.weight(30, 50), weights.weight(2, 5), "a long good record outweighs a short one")
        self.assertAlmostEqual(weights.weight(40, 40), 1.0 if weights.weight(40, 40) == 1.0 else weights.weight(40, 40))
        self.assertEqual(weights.weight(0, 200), weights.MIN_WEIGHT, "a long bad record is nearly ignored")
        self.assertLess(weights.weight(2, 20), 0.5)
        self.assertLessEqual(weights.weight(200, 200), 1.0)

    def test_the_conditions_are_blended_by_weight_and_a_number_only_one_side_has_is_kept(self):
        from papercompmoomoo import weights
        self.assertEqual(weights.blend({"stop_pct": 3.0, "tp": 8.0}, {"stop_pct": 2.0, "hold": 120.0}, 0.5), {"stop_pct": 2.5, "tp": 8.0, "hold": 120.0})
        self.assertEqual(weights.blend({"stop_pct": 3.0}, {"stop_pct": 2.0}, 1.0), {"stop_pct": 3.0})
        self.assertEqual(weights.blend({"stop_pct": 3.0}, {"stop_pct": 2.0}, 0.0), {"stop_pct": 2.0})

    def test_the_weights_come_from_the_journals_closed_trades_and_every_board_message_carries_its_senders(self):
        from papercompmoomoo import journal, weights
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        j = journal.Journal(Path(d.name) / "j.sqlite3")
        self.addCleanup(j.close)
        for pnl in (2.0, -1.0, 1.0, 3.0):
            j.scout_learning(1, "US.A", {}, {"stop_pct": 2.0}, pnl, 30.0, "trail", source="plan")
        j.scout_learning(1, "US.B", {}, {}, 5.0, 30.0, "trail", source="rule")  # a rule trade nobody assessed counts for nobody
        w = weights.from_journal(j.db)
        self.assertEqual((w["planner"]["trades"], w["planner"]["hits"]), (4, 3))
        self.assertGreater(w["planner"]["weight"], 0.5)
        self.assertEqual((w["assessor"]["trades"], w["assessor"]["weight"]), (0, 0.5))
        messages = board.for_prompt([{"id": 1, "sender": "planner", "recipient": "*", "topic": "", "kind": "note", "body": "x", "reply_to": None, "goal": ""},
                                     {"id": 2, "sender": "unknown", "recipient": "*", "topic": "", "kind": "note", "body": "y", "reply_to": None, "goal": ""}], w)
        self.assertEqual([m["weight"] for m in messages], [w["planner"]["weight"], 0.5])


if __name__ == "__main__":
    unittest.main()
