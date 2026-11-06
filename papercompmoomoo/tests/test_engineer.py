import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from papercompmoomoo import engineer as e


def sh(root, *args):
    subprocess.run(args, cwd=root, check=True, capture_output=True)


class Done:
    returncode = 0
    stdout = stderr = ""


class Engineering(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.root = Path(self.dir.name)
        (self.root / "src/papercompmoomoo").mkdir(parents=True)
        (self.root / "tests").mkdir()
        (self.root / "src/papercompmoomoo/owner_rules.py").write_text("LIMIT = 2\n")
        (self.root / "src/papercompmoomoo/thing.py").write_text("VALUE = 1\n")
        for cmd in (("git", "init", "-q"), ("git", "add", "-A"), ("git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "start")):
            sh(self.root, *cmd)
        self.start = e.git(self.root, "rev-parse", "HEAD").strip()
        self.find = lambda name: f"/bin/{name}"
        self.pass_tests = lambda root: (True, "")

    def agent(self, edit):
        def run(cmd, cwd=None, **kw):
            edit(Path(cwd))
            return Done()
        return run

    def job(self, edit, tests=None, agent="claude"):
        return e.work(self.root, agent, "make VALUE 2", self.find, run=self.agent(edit), tests=tests or self.pass_tests)

    def test_a_good_change_that_passes_the_tests_is_committed_by_the_agent(self):
        result = self.job(lambda r: (r / "src/papercompmoomoo/thing.py").write_text("VALUE = 2\n"))
        self.assertEqual((result["ok"], result["files"]), (True, ["src/papercompmoomoo/thing.py"]))
        self.assertNotEqual(e.git(self.root, "rev-parse", "HEAD").strip(), self.start)
        self.assertIn("claude", e.git(self.root, "log", "-1", "--format=%an"))

    def test_the_hard_rule_files_cannot_be_edited_and_the_tree_is_restored(self):
        result = self.job(lambda r: (r / "src/papercompmoomoo/owner_rules.py").write_text("LIMIT = 99\n"))
        self.assertFalse(result["ok"])
        self.assertIn("only the owner edits it", result["why"])
        self.assertEqual((self.root / "src/papercompmoomoo/owner_rules.py").read_text(), "LIMIT = 2\n")
        self.assertEqual(e.git(self.root, "rev-parse", "HEAD").strip(), self.start)

    def test_text_that_reaches_the_real_account_or_a_guard_is_refused(self):
        for bad in ("env = TrdEnv.REAL\n", "open('data/capital_resume','w')\n", "# --dangerously-skip-permissions\n"):
            result = self.job(lambda r, b=bad: (r / "src/papercompmoomoo/thing.py").write_text("VALUE = 1\n" + b))
            self.assertFalse(result["ok"], bad)
            self.assertEqual((self.root / "src/papercompmoomoo/thing.py").read_text(), "VALUE = 1\n")

    def test_new_files_are_scanned_too_and_places_outside_the_allowed_ones_are_refused(self):
        result = self.job(lambda r: (r / "src/papercompmoomoo/new.py").write_text("x = TrdEnv.REAL\n"))
        self.assertFalse(result["ok"])
        self.assertFalse((self.root / "src/papercompmoomoo/new.py").exists(), "the new file is removed")
        result = self.job(lambda r: (r / "outside.sh").write_text("echo hi\n"))
        self.assertIn("outside the places", result["why"])

    def test_failing_tests_undo_the_change(self):
        result = self.job(lambda r: (r / "src/papercompmoomoo/thing.py").write_text("VALUE = 2\n"), tests=lambda root: (False, "1 failed"))
        self.assertFalse(result["ok"])
        self.assertIn("tests fail", result["why"])
        self.assertEqual((self.root / "src/papercompmoomoo/thing.py").read_text(), "VALUE = 1\n")

    def test_uncommitted_work_is_never_touched_and_an_agent_that_changes_nothing_is_reported(self):
        self.assertIn("nothing", self.job(lambda r: None)["why"])
        (self.root / "src/papercompmoomoo/thing.py").write_text("VALUE = 5\n")
        result = self.job(lambda r: None)
        self.assertIn("uncommitted", result["why"])
        self.assertEqual((self.root / "src/papercompmoomoo/thing.py").read_text(), "VALUE = 5\n")

    def test_the_local_model_edits_only_through_the_harness_with_exact_matches_inside_allowed_places(self):
        ask = lambda prompt: json.dumps([{"path": "src/papercompmoomoo/thing.py", "old": "VALUE = 1", "new": "VALUE = 3"}])  # noqa: E731
        result = e.work(self.root, "local", "change src/papercompmoomoo/thing.py", self.find, local_ask=ask, tests=self.pass_tests)
        self.assertTrue(result["ok"], result)
        self.assertEqual((self.root / "src/papercompmoomoo/thing.py").read_text(), "VALUE = 3\n")
        for bad in ('[{"path": "src/papercompmoomoo/owner_rules.py", "old": "LIMIT = 2", "new": "LIMIT = 9"}]', '[{"path": "../x.py", "old": "a", "new": "b"}]',
                    '[{"path": "src/papercompmoomoo/thing.py", "old": "NOT THERE", "new": "b"}]', "garbage"):
            result = e.work(self.root, "local", "task", self.find, local_ask=lambda p, b=bad: b, tests=self.pass_tests)
            self.assertFalse(result["ok"], bad)
        self.assertEqual((self.root / "src/papercompmoomoo/owner_rules.py").read_text(), "LIMIT = 2\n")

    def test_the_agent_commands_are_limited_and_a_missing_agent_is_reported(self):
        claude = e.agent_command("claude", "t", self.find)
        self.assertIn("acceptEdits", claude)
        self.assertNotIn("--dangerously-skip-permissions", claude)
        self.assertIn("workspace-write", e.agent_command("codex", "t", self.find))
        self.assertIsNone(e.agent_command("codex", "t", lambda n: None))
        self.assertIn("not installed", e.work(self.root, "codex", "t", lambda n: None)["why"])

    def test_the_gate_protects_itself_and_its_tests(self):
        for path in ("src/papercompmoomoo/engineer.py", "tests/test_engineer.py", "src/papercompmoomoo/owner_rules.py", "src/papercompmoomoo/conduct.py"):
            self.assertIn(path, e.PROTECTED)


if __name__ == "__main__":
    unittest.main()
