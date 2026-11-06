import json
import tempfile
import unittest
from pathlib import Path

from papercompmoomoo import board, council, llm, rules

NOW = 1_800_000_000_000
RULE = {"op": "add_rule", "agent": "planner", "kind": "clamp", "text": "Stops at most 3.5%.", "evidence": "9 stopped-out trades", "param": "stop_pct", "min": None, "max": 3.5}


class Council(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.db = board.connect(Path(self.dir.name) / "b.sqlite3")
        self.addCleanup(self.db.close)

    def vote(self, did, stances, weights=None):
        for agent, stance in stances.items():
            council.cast(self.db, did, agent, stance, f"{agent} says {stance}", (weights or {}).get(agent, 0.5), via="test", now_ms=NOW)

    def test_a_decision_is_opened_on_the_board_and_bad_ones_are_refused(self):
        did = council.open_decision(self.db, "commander", "Stops", "Cap the planner's stop at 3.5%.", goal="O1", payload=RULE, now_ms=NOW)
        self.assertEqual([d["id"] for d in council.open_decisions(self.db)], [did])
        self.assertTrue(any("Decision #" in m["body"] and m["goal"] == "O1" for m in board.recent(self.db)))
        for kwargs in ({"proposer": "stranger", "title": "t", "proposal": "p"}, {"proposer": "commander", "title": "", "proposal": "p"}, {"proposer": "commander", "title": "t", "proposal": "p", "payload": {"op": "delete_everything"}}):
            with self.assertRaises(ValueError):
                council.open_decision(self.db, now_ms=NOW, **kwargs)

    def test_the_votes_are_weighted_by_track_record_and_a_quorum_is_needed(self):
        did = council.open_decision(self.db, "commander", "t", "p", payload=RULE, now_ms=NOW)
        self.vote(did, {"planner": "oppose", "assessor": "support", "reviewer": "support"}, {"planner": 1.0, "assessor": 0.3, "reviewer": 0.3})
        t = council.tally(self.db, did)
        self.assertEqual((t["support"], t["oppose"], t["voters"]), (0.6, 1.0, 3))
        self.assertEqual(council.verdict(t, True, False), "rejected", "the heavier record outweighs two light ones")
        did2 = council.open_decision(self.db, "commander", "t2", "p", payload=RULE, now_ms=NOW)
        self.vote(did2, {"planner": "support", "judge": "support"})
        self.assertEqual(council.verdict(council.tally(self.db, did2), True, False), "deferred", "two voters are under the quorum")
        self.assertIsNone(council.verdict(council.tally(self.db, did2), False, False), "still waiting for the others")
        self.vote(did2, {"reviewer": "support", "assessor": "oppose"}, {"assessor": 0.1})
        self.assertEqual(council.verdict(council.tally(self.db, did2), True, False), "approved")

    def test_abstentions_do_not_count_for_or_against_and_a_member_votes_once(self):
        did = council.open_decision(self.db, "commander", "t", "p", payload=RULE, now_ms=NOW)
        self.vote(did, {"planner": "support", "assessor": "abstain", "reviewer": "abstain", "judge": "oppose"}, {"planner": 0.9, "judge": 0.3})
        t = council.tally(self.db, did)
        self.assertEqual((t["share"], t["abstained"]), (0.75, ["assessor", "reviewer"]))
        self.assertFalse(council.cast(self.db, did, "planner", "oppose", "changed my mind", 0.9, now_ms=NOW), "one vote each")

    def test_stances_are_read_from_json_or_a_word_and_anything_else_is_an_abstention(self):
        self.assertEqual(council.parse_stance('{"stance": "SUPPORT", "reason": "evidence"}'), ("support", "evidence"))
        self.assertEqual(council.parse_stance("I oppose this: the sample is small."), ("oppose", "I oppose this: the sample is small."))
        self.assertEqual(council.parse_stance("hmm")[0], "abstain")
        self.assertEqual(council.parse_stance('{"stance": "maybe"}')[0], "abstain")

    def test_an_approved_rule_decision_makes_the_rule_a_rejected_one_does_not_and_the_outcome_is_on_the_board(self):
        weights = {a: {"weight": 0.5} for a in council.MEMBERS}
        did = council.open_decision(self.db, "commander", "Stops", "Cap at 3.5%.", goal="O1", payload=RULE, now_ms=NOW)

        def complete(prompt, order=None, now_ms=None, state_path=None, system="", **k):
            return llm.Result(json.dumps({"stance": "support", "reason": "the evidence is there"}), (order or ["claude"])[0], [])

        votes = council.convene(self.db, council.open_decisions(self.db)[0], weights, complete, NOW, ["claude", "codex", "omlx"], max_votes=10)
        self.assertEqual(votes, len(council.MEMBERS))
        self.assertEqual(council.settle(self.db, council.open_decisions(self.db)[0], NOW), "approved")
        self.assertEqual([r["text"] for r in rules.active_for(self.db, "planner")], ["Stops at most 3.5%."])
        last = [m for m in board.recent(self.db) if m["kind"] == "decision"][-1]
        self.assertIn("approved", last["body"])
        self.assertEqual(last["sender"], "commander")
        # a decision the council opposes changes nothing
        did2 = council.open_decision(self.db, "commander", "Other", "Cap trails.", payload={**RULE, "param": "trail_pct", "text": "Trails at most 2%.", "max": 2.0}, now_ms=NOW)

        def against(prompt, order=None, now_ms=None, state_path=None, system="", **k):
            return llm.Result('{"stance": "oppose", "reason": "too tight"}', "omlx", [])

        council.convene(self.db, council.open_decisions(self.db)[0], weights, against, NOW, ["omlx"], max_votes=10)
        self.assertEqual(council.settle(self.db, council.open_decisions(self.db)[0], NOW), "rejected")
        self.assertEqual(len(rules.active_for(self.db, "planner")), 1)
        self.assertEqual([d["status"] for d in council.recent_decisions(self.db)], ["decided", "decided"])

    def test_an_approved_operation_that_breaks_the_rules_checks_is_reported_not_applied_and_the_owner_can_overrule(self):
        did = council.open_decision(self.db, "commander", "Too wide", "Stops up to 40%.", payload={**RULE, "max": 40.0}, now_ms=NOW)
        self.vote(did, {"planner": "support", "assessor": "support", "reviewer": "support"})
        self.assertEqual(council.settle(self.db, council.open_decisions(self.db)[0], NOW + council.DEADLINE_MS + 1), "approved")
        self.assertEqual(rules.active_for(self.db, "planner"), [])
        self.assertIn("could not be carried out", council.recent_decisions(self.db)[0]["rationale"])
        did2 = council.open_decision(self.db, "commander", "t", "p", payload=RULE, now_ms=NOW)
        self.assertTrue(council.overrule(self.db, did2, "owner: no", now_ms=NOW))
        self.assertEqual(council.open_decisions(self.db), [])


if __name__ == "__main__":
    unittest.main()
