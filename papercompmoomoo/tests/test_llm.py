import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from papercompmoomoo import llm

NOW = 1_800_000_000_000


class FakeResponse:
    def __init__(self, text):
        self.text = text

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False

    def read(self):
        return json.dumps({"choices": [{"message": {"content": self.text}}]}).encode()


class Router(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.state = Path(self.dir.name) / "s.json"
        self.config = {"order": ["claude", "codex", "omlx"], "omlx_url": "http://x/v1", "omlx_model": "m", "omlx_key_file": str(Path(self.dir.name) / "nokey"), "omlx_max_tokens": 50}

    def runner(self, script):
        """A fake subprocess.run: script maps the executable's name to (returncode, stdout, stderr)."""
        calls = []

        def run(args, **k):
            name = Path(args[0]).name
            calls.append(name)
            code, out, err = script[name]
            return subprocess.CompletedProcess(args, code, stdout=out, stderr=err)

        run.calls = calls
        return run

    def test_limit_messages_are_recognised_and_a_good_answer_never_is(self):
        for text in ("Claude AI usage limit reached|1800003600", "You've hit your usage limit. Try again later.", "5-hour limit reached - resets 3pm", "Error 429 too many requests", "rate limit exceeded"):
            self.assertTrue(llm.is_limit(text, 1), text)
        self.assertFalse(llm.is_limit("Usage limit discussion in the answer", 0), "a successful call is never a limit")
        self.assertFalse(llm.is_limit("a normal error: file not found", 1))

    def test_the_reset_time_is_read_from_the_message_and_defaults_when_there_is_none(self):
        self.assertEqual(llm.reset_ms("Claude AI usage limit reached|1800003600", NOW), 1_800_003_600_000)
        self.assertEqual(llm.reset_ms("try again in 2 hours", NOW), NOW + 2 * 3_600_000)
        self.assertEqual(llm.reset_ms("try again in 45 minutes", NOW), NOW + 45 * 60_000)
        self.assertEqual(llm.reset_ms("limit reached", NOW), NOW + llm.DEFAULT_WAIT_MS)
        self.assertEqual(llm.reset_ms("limit|1999999999", NOW), NOW + llm.MAX_WAIT_MS, "never longer than the cap")

    def test_the_first_backend_answers_when_it_can(self):
        run = self.runner({"claude": (0, "from claude", "")})
        r = llm.complete("p", run=run, now_ms=NOW, state_path=self.state, config=self.config)
        self.assertEqual((r.backend, r.text), ("claude", "from claude"))
        self.assertEqual(run.calls, ["claude"])

    def test_a_session_limit_hands_the_same_call_to_codex_then_to_the_local_model_and_marks_the_limits(self):
        run = self.runner({"claude": (1, "", "Claude AI usage limit reached|1800003600"), "codex": (1, "", "You have hit your usage limit, try again in 3 hours")})
        r = llm.complete("p", run=run, opener=lambda req, timeout=0: FakeResponse("from the local model"), now_ms=NOW, state_path=self.state, config=self.config)
        self.assertEqual((r.backend, r.text), ("omlx", "from the local model"))
        self.assertEqual(run.calls, ["claude", "codex"])
        state = json.loads(self.state.read_text())
        self.assertEqual(state["claude"]["limited_until_ms"], 1_800_003_600_000)
        self.assertEqual(state["codex"]["limited_until_ms"], NOW + 3 * 3_600_000)
        self.assertTrue(any("claude hit its limit" in n for n in r.notes))

    def test_a_limited_backend_is_not_asked_again_until_its_reset_and_is_handed_back_to_after_it(self):
        run = self.runner({"claude": (1, "", "usage limit reached|1800003600"), "codex": (0, "from codex", "")})
        llm.complete("p", run=run, now_ms=NOW, state_path=self.state, config=self.config)
        self.assertEqual(run.calls, ["claude", "codex"])
        run.calls.clear()
        r = llm.complete("p", run=run, now_ms=NOW + 60_000, state_path=self.state, config=self.config)
        self.assertEqual((r.backend, run.calls), ("codex", ["codex"]), "claude is skipped while limited: no quota is spent asking")
        run.calls.clear()
        later = llm.reset_ms("x|1800003600", NOW) + 1000
        run2 = self.runner({"claude": (0, "claude again", "")})
        r = llm.complete("p", run=run2, now_ms=later, state_path=self.state, config=self.config)
        self.assertEqual(r.backend, "claude")
        self.assertIn("claude is back", r.notes)
        self.assertEqual(json.loads(self.state.read_text())["claude"]["limited_until_ms"], 0)

    def test_a_failure_that_is_not_a_limit_moves_on_without_marking_a_limit_and_when_nobody_can_it_raises(self):
        run = self.runner({"claude": (2, "", "boom"), "codex": (0, "", "")})
        def down(req, timeout=0):
            raise OSError("connection refused")
        with self.assertRaises(llm.Failed) as caught:
            llm.complete("p", run=run, opener=down, now_ms=NOW, state_path=self.state, config=self.config)
        self.assertIn("claude failed", str(caught.exception))
        self.assertIn("the local model did not answer", str(caught.exception))
        self.assertFalse(llm.limited_until(llm.load_state(self.state), "claude", NOW), "a crash is not a limit")

    def test_the_local_model_answer_is_cleaned_and_asked_with_thinking_off_and_the_key(self):
        key = Path(self.dir.name) / "key"
        key.write_text("secret\n")
        self.config["omlx_key_file"] = str(key)
        seen = {}

        def opener(req, timeout=0):
            seen["auth"] = req.get_header("Authorization")
            seen["body"] = json.loads(req.data.decode())
            return FakeResponse("<think>hmm</think>the answer")

        r = llm.complete("p", order=["omlx"], opener=opener, now_ms=NOW, state_path=self.state, config=self.config, system="You are the reviewer.")
        self.assertEqual(r.text, "the answer")
        self.assertEqual(seen["auth"], "Bearer secret")
        self.assertEqual(seen["body"]["chat_template_kwargs"], {"enable_thinking": False})
        self.assertEqual(seen["body"]["messages"][0], {"role": "system", "content": "You are the reviewer."})

    def test_linux_local_llama_server_is_a_first_class_backend(self):
        config = {"order": ["local"], "local_url": "http://local/v1", "local_model": "qwen2.5-7b-instruct", "local_max_tokens": 50}
        seen = {}
        def opener(req, timeout=0):
            seen["url"] = req.full_url
            seen["body"] = json.loads(req.data.decode())
            return FakeResponse("local evidence")
        result = llm.complete("question", opener=opener, now_ms=NOW, state_path=self.state, config=config)
        self.assertEqual((result.backend, result.text), ("local", "local evidence"))
        self.assertEqual(seen["url"], "http://local/v1/chat/completions")
        self.assertEqual(seen["body"]["model"], "qwen2.5-7b-instruct")

    def test_a_limit_notice_printed_as_the_answer_with_exit_code_zero_is_still_a_limit(self):
        run = self.runner({"claude": (0, "You've hit your usage limit.", ""), "codex": (0, "from codex", "")})
        r = llm.complete("p", run=run, now_ms=NOW, state_path=self.state, config=self.config)
        self.assertEqual(r.backend, "codex")


    def test_every_backend_gets_the_compact_instructions_in_front_of_the_prompt(self):
        from papercompmoomoo import agents
        seen = {}

        def opener(req, timeout=0):
            seen["system"] = json.loads(req.data.decode())["messages"][0]["content"]
            return FakeResponse("local answer")

        llm.complete("QUESTION", order=["omlx"], opener=opener, now_ms=NOW, state_path=self.state, config=self.config, system="FULL TEXT", system_local="COMPACT TEXT")
        self.assertEqual(seen["system"], "COMPACT TEXT")
        sent = {}

        def run(args, **k):
            sent["prompt"] = args[args.index("-p") + 1] if "-p" in args else args[-1]
            return subprocess.CompletedProcess(args, 0, stdout="cli answer", stderr="")

        llm.complete("QUESTION", order=["claude"], run=run, now_ms=NOW, state_path=self.state, config=self.config, system="FULL TEXT", system_local="COMPACT TEXT")
        self.assertTrue(sent["prompt"].startswith("COMPACT TEXT") and sent["prompt"].endswith("QUESTION"), "the CLI backends get the compact instructions too: fewer tokens")
        for role in agents.ROLES:
            compact, full = agents.compact_prompt(role), agents.system_prompt(role)
            for rule in ("H1", "H2", "H3", "H4", "H5", "H6"):
                self.assertIn(rule, compact, f"{role}: the compact text keeps every hard rule")
            self.assertLess(len(compact), len(full) * 0.8)


    def test_the_daily_budget_of_background_calls_sends_the_work_on_and_routine_work_uses_the_cheaper_model(self):
        budget = Path(self.dir.name) / "budget.json"
        self.config.update({"claude_daily_calls": 2, "codex_daily_calls": 1})
        models = []

        def run(args, **k):
            name = Path(args[0]).name
            if name == "claude":
                models.append(args[args.index("--model") + 1])
            return subprocess.CompletedProcess(args, 0, stdout=f"from {name}", stderr="")

        got = [llm.complete("p", order=["claude", "codex"], run=run, now_ms=NOW, state_path=self.state, config=self.config, budget_path=budget).backend for _ in range(3)]
        self.assertEqual(got, ["claude", "claude", "codex"], "two background calls on Claude, then its allowance is used and the work moves on")
        with self.assertRaises(llm.Failed):
            llm.complete("p", order=["claude", "codex"], run=run, now_ms=NOW, state_path=self.state, config=self.config, budget_path=budget)
        self.assertEqual(set(models), {llm.CLAUDE_ROUTINE_MODEL}, "routine work runs on the cheaper model")
        llm.complete("p", order=["claude"], run=run, now_ms=NOW + 86_400_000 * 2, state_path=self.state, config=self.config, budget_path=budget, tier="important")
        self.assertEqual(models[-1], llm.CLAUDE_MODEL, "the important work runs on the stronger model, and a new day starts a new budget")
        self.assertGreater(json.loads(budget.read_text())["claude"]["tokens"], 0)


class ModelReady(unittest.TestCase):
    def test_a_model_is_ready_only_when_it_is_complete_and_a_prediction_head_is_recognised(self):
        with tempfile.TemporaryDirectory() as root:
            cfg = {"omlx_models_dir": root, "omlx_model": "m"}
            self.assertFalse(llm.model_ready(cfg), "not there")
            d = Path(root) / "m"
            d.mkdir()
            (d / "model.safetensors.index.json").write_text(json.dumps({"weight_map": {"a": "s1.safetensors", "mtp.fc.weight": "s2.safetensors"}}))
            (d / "s1.safetensors").write_bytes(b"x")
            self.assertFalse(llm.model_ready(cfg), "a shard is still missing")
            (d / "s2.safetensors").write_bytes(b"x")
            self.assertTrue(llm.model_ready(cfg))
            self.assertTrue(llm.has_prediction_head(cfg))
            (d / ".cache").mkdir()
            (d / ".cache" / "x.incomplete").write_bytes(b"y")
            self.assertFalse(llm.model_ready(cfg), "a download is still going")

    def test_the_local_backend_does_not_ask_a_model_that_is_still_downloading_and_the_router_moves_on(self):
        with tempfile.TemporaryDirectory() as root:
            cfg = {"order": ["omlx"], "omlx_url": "http://x/v1", "omlx_model": "m", "omlx_key_file": "none", "omlx_max_tokens": 5, "omlx_models_dir": root, "omlx_check_ready": True}
            asked = []
            with self.assertRaises(llm.Failed) as caught:
                llm.complete("p", order=["omlx"], opener=lambda *a, **k: asked.append(1), now_ms=NOW, state_path=Path(root) / "s.json", config=cfg)
            self.assertIn("still downloading", str(caught.exception))
            self.assertEqual(asked, [], "the server was not asked")


if __name__ == "__main__":
    unittest.main()
