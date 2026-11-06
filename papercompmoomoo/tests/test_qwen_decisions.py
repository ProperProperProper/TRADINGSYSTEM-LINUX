import json
import tempfile
import unittest
from unittest.mock import patch

from papercompmoomoo import llm
from tools import qwen_decisions, thomasai_local_decisions


class QwenDecisionAdapterTests(unittest.TestCase):
    def event(self):
        return {
            "schema": 1, "strategy": "cross-sectional-momentum-1m:v1:lookback20",
            "session": "2026-10-23", "signal_ts_ms": 100,
            "symbol": {"moomoo": "US.AAA", "yahoo": "AAA"},
            "rank": 2, "score": 0.02, "observations": 20,
            "data_cutoff": "20748", "future_outcome": 9.9,
        }

    def test_parser_accepts_only_known_json_wrappers(self):
        self.assertEqual(
            qwen_decisions.parse_decision_object(
                '<think>internal</think>\n```json\n{"decision":"abstain"}\n```'
            ),
            {"decision": "abstain"},
        )
        with self.assertRaises((ValueError, json.JSONDecodeError)):
            qwen_decisions.parse_decision_object("I recommend abstaining.")

    def test_local_adapter_uses_only_the_frozen_momentum_fields(self):
        prompt = thomasai_local_decisions.prompt_for(self.event())
        self.assertIn("FROZEN_MOMENTUM_CANDIDATE", prompt)
        self.assertIn('"rank":2', prompt)
        self.assertNotIn("future_outcome", prompt)
        self.assertNotIn("CLOSED_1M_EVENT", prompt)

    def test_local_adapter_version_is_the_weight_content_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            # Use pathlib directly so model identity changes with the actual SafeTensors bytes.
            from pathlib import Path
            adapter = Path(directory)
            (adapter / "adapters.safetensors").write_bytes(b"one")
            first = thomasai_local_decisions.adapter_version(adapter)
            (adapter / "adapters.safetensors").write_bytes(b"two")
            self.assertNotEqual(first, thomasai_local_decisions.adapter_version(adapter))

    @patch("tools.qwen_decisions.authoritative_metadata")
    @patch("tools.qwen_decisions.llm.complete")
    def test_forward_bars_are_not_prompted_and_backend_is_recorded(self, complete, metadata):
        metadata.return_value = {
            "symbol": {"moomoo": "US.AAA", "yahoo": "AAA"},
            "signal_ts_ms": 100, "brain_ids": [1], "board_ids": [],
            "data_cutoff": "100", "strategy_version": "1", "model_version": "",
        }
        complete.return_value = llm.Result(
            '{"decision":"abstain","confidence":0.9,"reason":"not enough evidence"}',
            "codex",
        )
        result = qwen_decisions.ask(self.event(), {"omlx_max_tokens": 160}, "100")
        prompt = complete.call_args.args[0]
        self.assertNotIn("future_outcome", prompt)
        self.assertIn("FROZEN_MOMENTUM_CANDIDATE", prompt)
        self.assertIn("enter or abstain", prompt)
        self.assertNotIn("skip", prompt)
        self.assertNotIn("exit", prompt)
        self.assertEqual(result["model_version"], "codex")
        self.assertEqual(result["decision"], "abstain")

    @patch("tools.qwen_decisions.authoritative_metadata")
    @patch("tools.qwen_decisions.llm.complete")
    def test_invalid_decision_is_rejected(self, complete, metadata):
        metadata.return_value = {"symbol": {}, "signal_ts_ms": 100, "brain_ids": [1], "board_ids": [], "data_cutoff": "100", "strategy_version": "1", "model_version": ""}
        complete.return_value = llm.Result(json.dumps({"decision": "buy", "confidence": 1, "reason": "x"}), "omlx")
        with self.assertRaises(ValueError):
            qwen_decisions.ask(self.event(), {}, "100")

    @patch("tools.qwen_decisions.authoritative_metadata")
    @patch("tools.qwen_decisions.llm.complete")
    def test_unsupported_runtime_actions_are_rejected(self, complete, metadata):
        metadata.return_value = {"symbol": {}, "signal_ts_ms": 100, "brain_ids": [1], "board_ids": [], "data_cutoff": "100", "strategy_version": "1", "model_version": ""}
        for action in ("skip", "exit"):
            complete.return_value = llm.Result(json.dumps({"decision": action, "confidence": 1, "reason": "x"}), "codex")
            with self.subTest(action=action), self.assertRaises(ValueError):
                qwen_decisions.ask(self.event(), {}, "100")

    @patch("tools.qwen_decisions.authoritative_metadata")
    @patch("tools.qwen_decisions.llm.complete")
    def test_local_backend_is_labelled_with_loaded_base_model(self, complete, metadata):
        metadata.return_value = {
            "symbol": {"moomoo": "US.AAA", "yahoo": "AAA"},
            "signal_ts_ms": 100, "brain_ids": [1], "board_ids": [],
            "data_cutoff": "100", "strategy_version": "1", "model_version": "",
        }
        complete.return_value = llm.Result(
            '{"decision":"abstain","confidence":0.5,"reason":"insufficient evidence"}',
            "omlx",
        )
        result = qwen_decisions.ask(self.event(), {"omlx_model": "base-qwen"}, "100")
        self.assertEqual(result["model_version"], "omlx:base-qwen")


if __name__ == "__main__":
    unittest.main()
