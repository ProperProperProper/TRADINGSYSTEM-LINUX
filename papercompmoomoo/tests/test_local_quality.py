import json
import os
import tempfile
import unittest
from pathlib import Path

from papercompmoomoo import board, local_quality as q, standby


class Garbage(unittest.TestCase):
    def test_empty_repeated_unprintable_and_unparseable_answers_are_garbage_and_real_answers_are_not(self):
        self.assertEqual(q.is_garbage(""), "empty")
        self.assertEqual(q.is_garbage("a"), "empty")
        self.assertEqual(q.is_garbage("the the the the the the the the the the the the the the"), "one or two words repeated")
        self.assertEqual(q.is_garbage("ok\nok\nok\nok\nok\nok\nok"), "the same line repeated")
        self.assertEqual(q.is_garbage("abcabcabcabcabcabcabcabc"), "a short pattern repeated")
        self.assertEqual(q.is_garbage("\x00\x01\x02\x03\x04\x05\x06\x07abc"), "unprintable characters")
        self.assertEqual(q.is_garbage("Sure, here you go", expect_json=True), "no JSON")
        self.assertIsNotNone(q.is_garbage('{"stance": "support", "reason": ', expect_json=True), "a cut-off JSON is garbage")
        self.assertIsNone(q.is_garbage("A trailing stop follows the price up and sells when it falls back by a set amount."))
        self.assertIsNone(q.is_garbage('Here it is: {"stance": "support", "reason": "evidence"}', expect_json=True))
        self.assertEqual(q.is_garbage('{"a": 1,, }', expect_json=True), "JSON that does not parse")


class Correcting(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.db = board.connect(Path(self.dir.name) / "b.sqlite3")
        self.addCleanup(self.db.close)

    def test_json_with_talk_fences_and_trailing_commas_is_repaired_without_asking_again(self):
        self.assertEqual(json.loads(q.repair_json('Sure!\n```json\n{"a": 1, "b": [1, 2,],}\n```\nHope it helps')), {"a": 1, "b": [1, 2]})
        self.assertIsNone(q.repair_json("no json at all"))
        asked = []
        fixed, reason, remedy = q.correct(self.db, 'Sure: {"a": 1,}', True, asked.append, "p")
        self.assertEqual((json.loads(fixed), remedy, asked), ({"a": 1}, "repair", []))

    def test_an_unusable_answer_is_asked_again_with_what_was_wrong_and_the_remedy_that_worked_is_remembered(self):
        seen = []

        def ask(p):
            seen.append(p)
            return "A proper answer with several real words."

        fixed, reason, remedy = q.correct(self.db, "", False, ask, "Question?")
        self.assertEqual((fixed, reason, remedy), ("A proper answer with several real words.", "empty", "reask"))
        self.assertIn("could not be used (empty)", seen[0])
        self.assertEqual(self.db.execute("SELECT remedy, success FROM local_remedies").fetchall(), [("reask", 1)])

    def test_when_nothing_works_the_result_is_none_so_the_next_backend_answers_and_the_remedies_are_ranked_by_what_worked(self):
        fixed, reason, remedy = q.correct(self.db, "", True, lambda p: "", "Q")
        self.assertEqual((fixed, remedy), (None, None))
        for _ in range(3):
            q.record_remedy(self.db, "no JSON", "reask", True)
            q.record_remedy(self.db, "no JSON", "repair", False)
        self.assertEqual(q.remedy_order(self.db, "no JSON"), ["reask", "repair"])
        self.assertEqual(q.remedy_order(self.db, "unseen"), ["repair", "reask"])

    def test_a_server_error_while_asking_again_is_not_fatal(self):
        def down(p):
            raise OSError("409")

        self.assertEqual(q.correct(self.db, "", False, down, "Q")[0], None)


class Learning(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.db = board.connect(Path(self.dir.name) / "b.sqlite3")
        self.addCleanup(self.db.close)
        self.on = {"mtp": 1, "ane": 3, "ane_dual": 0}

    def feed(self, features, answers):
        for good in answers:
            q.record(self.db, features, "A fine answer with several real words in it." if good else "", False)

    def test_bad_answers_turn_the_riskiest_setting_down_one_level_and_nothing_is_banned(self):
        self.feed(self.on, [True, True])
        self.assertIsNone(q.verdict(self.db, self.on), "too few answers to judge")
        self.feed(self.on, [False, False, False])
        state: dict = {}
        levels, action = q.adapt(self.db, self.on, state, 1000)
        self.assertEqual((action["action"], action["setting"], levels["ane"], levels["mtp"]), ("down", "ane", 2, 1))
        self.assertEqual((state["ane"]["ceiling"], state["ane"]["failures"], state["ane"]["retry_after_ms"]), (3, 1, 1000 + q.COOLDOWN_BASE_MS))
        self.assertNotIn("banned", json.dumps(state))
        # garbage at every ane level takes it to 0, then prediction goes down, then nothing is left
        cur = levels
        for _ in range(2):
            self.feed(cur, [False] * 3)
            cur, act = q.adapt(self.db, cur, state, 2000)
        self.assertEqual(cur["ane"], 0)
        self.feed(cur, [False] * 3)
        cur, act = q.adapt(self.db, cur, state, 3000)
        self.assertEqual((act["setting"], cur["mtp"]), ("mtp", 0))
        self.feed(cur, [False] * 3)
        _, act = q.adapt(self.db, cur, state, 4000)
        self.assertEqual(act["action"], "none-left")

    def test_a_turned_down_setting_is_tried_again_after_the_cool_down_and_good_answers_and_waits_twice_as_long_when_it_fails_again(self):
        state: dict = {}
        self.feed(self.on, [False] * 3)
        down, _ = q.adapt(self.db, self.on, state, 0)
        # still cooling down
        self.feed(down, [True] * 25)
        self.assertIsNone(q.adapt(self.db, down, state, q.COOLDOWN_BASE_MS - 1)[1])
        self.assertEqual(q.start_levels(self.on, state, q.COOLDOWN_BASE_MS - 1)["ane"], 2, "a restart keeps it below the failed level during the cool-down")
        up, action = q.adapt(self.db, down, state, q.COOLDOWN_BASE_MS)
        self.assertEqual((action["action"], up["ane"]), ("up", 3))
        self.assertEqual(q.start_levels(self.on, state, q.COOLDOWN_BASE_MS)["ane"], 3)
        self.feed(up, [False] * 3)
        again, action = q.adapt(self.db, up, state, q.COOLDOWN_BASE_MS + 10)
        self.assertEqual((action["action"], again["ane"]), ("down", 2))
        self.assertEqual(state["ane"]["retry_after_ms"], q.COOLDOWN_BASE_MS + 10 + 2 * q.COOLDOWN_BASE_MS)

    def test_answers_from_an_earlier_run_of_the_same_configuration_do_not_condemn_the_retry(self):
        state: dict = {}
        self.feed(self.on, [False] * 3)  # the first run of this configuration failed (ts = now)
        down, _ = q.adapt(self.db, self.on, state, 0)
        later = int(__import__("time").time() * 1000) + 1000
        # the setting is tried again: nothing has been answered since it was applied, so there is nothing to judge yet
        self.assertIsNone(q.adapt(self.db, self.on, state, q.COOLDOWN_BASE_MS, later)[1])
        self.assertEqual(q.good_since_failure(self.db, self.on, later), 0)

    def test_too_few_good_answers_do_not_earn_a_retry(self):
        state = {"ane": {"ceiling": 3, "failures": 1, "retry_after_ms": 0}}
        low = {"mtp": 1, "ane": 2, "ane_dual": 0}
        self.feed(low, [True] * (q.GOOD_BEFORE_RETRY - 1))
        self.assertIsNone(q.adapt(self.db, low, state, 10)[1])
        self.feed(low, [True])
        self.assertEqual(q.adapt(self.db, low, state, 10)[1]["action"], "up")

    def test_one_bad_answer_in_a_small_sample_is_nothing_but_a_rate_over_enough_answers_is_something(self):
        self.feed(self.on, [True, True, True, False, True])
        self.assertIsNone(q.verdict(self.db, self.on), "1 of 5 is 20% but only five answers: not evidence")
        other = {"mtp": 0, "ane": 3, "ane_dual": 0}
        self.feed(other, [True, False, True, True, False, True, True, True, True, True])
        self.assertIsNotNone(q.verdict(self.db, other), "2 of 10 over ten answers is a rate worth acting on")
        self.feed(self.on, [True] * 19 + [False])
        self.assertIsNone(q.verdict(self.db, self.on), "1 of 20 is 5%")

    def test_a_server_that_does_not_answer_is_not_counted_as_garbage_and_the_probe_waits_for_it(self):
        calls = {"n": 0}

        def flaky(prompt):
            calls["n"] += 1
            if calls["n"] <= 2:
                raise OSError("409 conflict: the model is reloading")
            return "ready" if "ready" in prompt else ("A real answer with several words." if "trailing" in prompt else '{"stance": "support", "reason": "ok"}' if "stance" in prompt else "[2, 4]")

        slept = []
        self.assertTrue(q.wait_until_answering(flaky, attempts=5, pause_s=1.0, sleep=slept.append))
        self.assertEqual(len(slept), 2, "it waited while the server answered nothing")
        self.assertEqual(q.probe(flaky, self.db, self.on, 1), 0)
        self.assertEqual(self.db.execute("SELECT count(*) FROM local_calls").fetchone()[0], 3)

        def down(prompt):
            raise OSError("nothing")

        other = {"mtp": 0, "ane": 3, "ane_dual": 0}
        self.assertEqual(q.probe(down, self.db, other, 2), 0, "errors are not garbage")
        self.assertEqual(self.db.execute("SELECT count(*) FROM local_calls WHERE config_key=?", (q.config_key(other),)).fetchone()[0], 0)
        self.assertFalse(q.wait_until_answering(down, attempts=2, pause_s=0, sleep=lambda s: None))

    def test_the_commander_turns_the_setting_down_pushes_it_tells_the_board_and_writes_a_lesson_to_the_brain(self):
        from papercompmoomoo import brain, llm
        old = (q.CONFIG_FILE, q.STATE_FILE)
        q.CONFIG_FILE, q.STATE_FILE = Path(self.dir.name) / "cfg.json", Path(self.dir.name) / "state.json"
        self.addCleanup(lambda: (setattr(q, "CONFIG_FILE", old[0]), setattr(q, "STATE_FILE", old[1])))
        q.CONFIG_FILE.write_text(json.dumps({"features": self.on}))
        self.feed(self.on, [False, False, False])
        sent = {}

        class Opener:
            def __call__(self, request, timeout=0):
                sent["body"] = json.loads(request.data.decode())

                class R:
                    def __enter__(s): return s
                    def __exit__(s, *a): return False
                    def read(s): return b"{}"
                return R()

        key = Path(self.dir.name) / "key"
        key.write_text("k")
        real = llm.load_config
        llm.load_config = lambda path=None: {**real(Path(self.dir.name) / "none.json"), "order": ["omlx"], "omlx_url": "http://x/v1", "omlx_model": "m", "omlx_max_tokens": 50, "omlx_key_file": str(key), "omlx_check_ready": False}
        self.addCleanup(setattr, llm, "load_config", real)
        s = standby.Standby(self.db, complete=None, now=lambda: 5000, status_file=Path(self.dir.name) / "s.json")
        s.brain_path = Path(self.dir.name) / "brain.sqlite3"
        action = s.learn_local({"omlx": {"status": "ok"}}, opener=Opener())
        self.assertEqual((action["action"], action["setting"]), ("down", "ane"))
        self.assertTrue(sent["body"]["qwen35_ane_prefill_enabled"], "still on, at a lower level")
        self.assertEqual(sent["body"]["qwen35_ane_prefill_max_layers"], 32)
        self.assertTrue(sent["body"]["mtp_enabled"])
        self.assertEqual(q.load_state()["ane"]["ceiling"], 3)
        alert = [m for m in board.recent(self.db) if m["topic"] == "local model"][0]
        self.assertIn("turned down", alert["body"])
        self.assertNotIn("ban", alert["body"].lower())
        bdb = brain.connect(s.brain_path)
        self.assertEqual(bdb.execute("SELECT count(*) FROM memories WHERE topic='local model'").fetchone()[0], 1)
        bdb.close()


if __name__ == "__main__":
    unittest.main()
