import sqlite3
import unittest

from papercompmoomoo import board, validation


class Validation(unittest.TestCase):
    def setUp(self):
        self.db = board.connect(":memory:")

    def tearDown(self):
        self.db.close()

    def test_records_backend_validator_and_evidence_links(self):
        item = validation.record(self.db, subject="entry hypothesis", primary_backend="omlx", validator="claude",
                                 verdict="revise", finding="Needs a held-out test.", evidence="three prior blocks",
                                 brain_ids=[4], board_ids=[8], rust_status="pending", model_version="qwen-base",
                                 strategy_version="v2", now_ms=7)
        row = validation.recent(self.db)[0]
        self.assertEqual((item, row["primary_backend"], row["validator"], row["verdict"]), (1, "omlx", "claude", "revise"))
        self.assertEqual((row["brain_ids"], row["board_ids"], row["rust_status"]), ("[4]", "[8]", "pending"))

    def test_rejects_unknown_validator_or_verdict(self):
        for kwargs in ({"validator": "omlx", "verdict": "accept"}, {"validator": "codex", "verdict": "maybe"}):
            with self.assertRaises(ValueError):
                validation.record(self.db, subject="x", primary_backend="omlx", finding="y", **kwargs)

    def test_parser_accepts_only_the_bounded_validator_envelope(self):
        parsed = validation.parse_response('{"verdict":"accept","finding":"Evidence agrees.","evidence":"two folds"}')
        self.assertEqual(parsed["verdict"], "accept")
        self.assertEqual(parsed["evidence"], "two folds")
        with self.assertRaises(ValueError):
            validation.parse_response('{"verdict":"accept","finding":""}')

    def test_request_records_valid_answer_and_invalid_answer_is_uncertain(self):
        first = validation.request(self.db, lambda prompt: '{"verdict":"accept","finding":"Matches the held-out result."}',
                                   subject="proposal", primary_backend="omlx", validator="codex", proposal="try it", now_ms=8)
        second = validation.request(self.db, lambda prompt: "not json", subject="proposal", primary_backend="omlx",
                                    validator="claude", proposal="try it", now_ms=9)
        rows = {row["id"]: row for row in validation.recent(self.db, "proposal")}
        self.assertEqual(rows[first]["verdict"], "accept")
        self.assertEqual(rows[second]["verdict"], "uncertain")
        messages = self.db.execute("SELECT topic, via FROM messages ORDER BY id").fetchall()
        self.assertTrue(messages and messages[-1] == ("model validation", "claude"))

    def test_only_explicit_rust_statuses_can_update_a_record(self):
        item = validation.record(self.db, subject="x", primary_backend="omlx", validator="codex",
                                 verdict="accept", finding="checked", now_ms=1)
        validation.set_rust_status(self.db, item, "pass")
        self.assertEqual(validation.recent(self.db)[0]["rust_status"], "pass")
        with self.assertRaises(ValueError):
            validation.set_rust_status(self.db, item, "accept")
